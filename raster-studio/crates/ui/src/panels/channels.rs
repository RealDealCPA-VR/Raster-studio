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
//! What this build still does not have is per-channel *editing* — painting into
//! the red channel alone. The panel's selection is therefore an isolation
//! target, not a paint target; `docs/parity-matrix.md` carries that gap.
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
}

/// Channel visibility, which is a view setting rather than document state.
#[derive(Clone, PartialEq, Debug)]
pub struct ChannelsState {
    /// One flag per colour component; `true` means the component contributes.
    components: [bool; 4],
    /// The row the user has selected for editing.
    pub selected: ChannelKind,
}

impl Default for ChannelsState {
    fn default() -> Self {
        Self {
            components: [true; 4],
            selected: ChannelKind::Composite,
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
        let names = component_names(mode);
        let mut rows = vec![ChannelRow {
            name: composite_name(mode).to_string(),
            kind: ChannelKind::Composite,
            visible: self.composite_visible(mode),
            shortcut_digit: Some(2),
        }];
        for (i, name) in names.iter().enumerate() {
            rows.push(ChannelRow {
                name: (*name).to_string(),
                kind: ChannelKind::Component(i),
                visible: self.component_visible(i),
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
            // A mask channel's visibility is the mask's own `enabled` flag,
            // which is document state; isolating one is a selection only.
            ChannelKind::Mask { .. } => {}
        }
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

    /// Drop a selection whose layer has left the document.
    pub fn prune(&mut self, doc: &Document) {
        if let Some(id) = self.selected {
            if !doc.layers.contains(id) {
                self.selected = None;
            }
        }
    }
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
