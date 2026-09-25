//! W10-B: the document-level records that are not layers.
//!
//! Four Photopea panels keep their state *in the document* rather than in a
//! layer or in the preferences:
//!
//! * **Layer Comps** — named snapshots of every layer's visibility, position
//!   and appearance (opacity, fill, blend mode, layer style). Applying one
//!   puts those properties back; it never adds or removes a layer.
//! * **Notes** — text pinned at a document position. A note is annotation, not
//!   content: nothing here is a layer, so no compositor and no exporter ever
//!   sees one.
//! * **Character Styles** and **Paragraph Styles** — named text settings
//!   applied to text layers. The link from a layer to the style it wears is
//!   kept here too ([`StyleLink`]), so redefining a style can update every
//!   layer that uses it.
//! * **The alpha channel being edited** — a saved selection opened as a
//!   grayscale channel in the Channels panel ([`AlphaEdit`]).
//!
//! Everything is `#[serde(default)]` and appended only: a document written
//! before this module loads with an empty [`DocumentExtras`], and one written
//! by a build that knows more fields keeps loading here.

use serde::{Deserialize, Serialize};

use crate::blend::BlendMode;
use crate::effects::LayerEffects;
use crate::ids::LayerId;
use crate::layer::{LayerKind, TextLayer};
use crate::text::{BaseStyle, Paragraph};
use crate::tree::LayerTree;

/// Every document-level record the W10-B panels keep.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DocumentExtras {
    /// The Layer Comps panel's comps, in panel order.
    pub layer_comps: Vec<LayerComp>,
    /// The comp most recently applied, by index into `layer_comps` —
    /// Photopea's marker in the comp list, and what Previous / Next step
    /// from.
    pub last_comp: Option<usize>,
    /// The Notes panel's notes, oldest first.
    pub notes: Vec<Note>,
    /// Named character styles.
    pub character_styles: Vec<CharacterStyle>,
    /// Named paragraph styles.
    pub paragraph_styles: Vec<ParagraphStyle>,
    /// Which text layer wears which style.
    pub style_links: Vec<StyleLink>,
    /// A saved selection currently open for editing as an alpha channel.
    pub alpha_edit: Option<AlphaEdit>,
    /// W11-E: the Layers panel's colour labels, one row per labelled layer
    /// (see [`crate::color_label`]). Appended; a document written before it
    /// loads with no labels.
    pub layer_colors: Vec<crate::color_label::LayerColorLabel>,
    /// W16-E: the Layer Comps panel's Last Document State — every layer as
    /// it stood before a comp was applied from it, so the panel's top row
    /// can put the document back. `None` until a comp is first applied.
    /// Appended; a document written before it loads with none.
    pub last_document_state: Option<LayerComp>,
}

impl DocumentExtras {
    /// `true` when nothing is stored, so the document can omit the field.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// A fresh id for a note or a style: one past the largest in use, so ids
    /// never repeat within a document.
    pub fn next_id(&self) -> u64 {
        let notes = self.notes.iter().map(|n| n.id);
        let chars = self.character_styles.iter().map(|s| s.id);
        let paras = self.paragraph_styles.iter().map(|s| s.id);
        notes.chain(chars).chain(paras).max().map_or(1, |m| m + 1)
    }

    /// The link row for `layer`, if it wears any style.
    pub fn link(&self, layer: LayerId) -> Option<&StyleLink> {
        self.style_links.iter().find(|l| l.layer == layer)
    }

    /// Set (or clear, with `None`) the character style `layer` wears.
    pub fn link_character(&mut self, layer: LayerId, style: Option<u64>) {
        self.link_mut(layer).character = style;
        self.prune_links();
    }

    /// Set (or clear, with `None`) the paragraph style `layer` wears.
    pub fn link_paragraph(&mut self, layer: LayerId, style: Option<u64>) {
        self.link_mut(layer).paragraph = style;
        self.prune_links();
    }

    fn link_mut(&mut self, layer: LayerId) -> &mut StyleLink {
        if let Some(i) = self.style_links.iter().position(|l| l.layer == layer) {
            return &mut self.style_links[i];
        }
        self.style_links.push(StyleLink {
            layer,
            character: None,
            paragraph: None,
        });
        let last = self.style_links.len() - 1;
        &mut self.style_links[last]
    }

    /// Drop link rows that name no style at all.
    fn prune_links(&mut self) {
        self.style_links
            .retain(|l| l.character.is_some() || l.paragraph.is_some());
    }

    /// The text layers wearing character style `id`, in link order.
    pub fn layers_with_character(&self, id: u64) -> Vec<LayerId> {
        self.style_links
            .iter()
            .filter(|l| l.character == Some(id))
            .map(|l| l.layer)
            .collect()
    }

    /// The text layers wearing paragraph style `id`, in link order.
    pub fn layers_with_paragraph(&self, id: u64) -> Vec<LayerId> {
        self.style_links
            .iter()
            .filter(|l| l.paragraph == Some(id))
            .map(|l| l.layer)
            .collect()
    }
}

/// One layer's recorded state inside a [`LayerComp`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompLayerState {
    pub layer: LayerId,
    /// Visibility.
    pub visible: bool,
    /// Position: the layer's whole layer-to-document transform, as the six
    /// column-major affine components (`glam::Affine2::to_cols_array`).
    pub transform: [f32; 6],
    /// Appearance.
    pub opacity: f32,
    #[serde(default = "one")]
    pub fill_opacity: f32,
    pub blend_mode: BlendMode,
    #[serde(default)]
    pub effects: LayerEffects,
}

fn one() -> f32 {
    1.0
}

/// A named snapshot of every layer's visibility, position and appearance.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayerComp {
    pub name: String,
    /// Photopea's comp comment.
    pub comment: String,
    pub layers: Vec<CompLayerState>,
    /// W16-E: which of the recorded aspects Apply puts back — Photopea's
    /// Visibility, Position and Appearance flags on each comp. All on by
    /// default, so a comp written before the flags applies as it always did.
    pub flags: CompFlags,
}

/// W16-E: the three aspects of a [`LayerComp`] that applying it restores.
/// A cleared flag leaves that aspect of every layer as it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CompFlags {
    /// Each layer's visibility.
    pub visibility: bool,
    /// Each layer's position (its layer-to-document transform).
    pub position: bool,
    /// Each layer's opacity, fill, blend mode and layer style.
    pub appearance: bool,
}

impl Default for CompFlags {
    fn default() -> Self {
        Self {
            visibility: true,
            position: true,
            appearance: true,
        }
    }
}

impl LayerComp {
    /// Record every layer of `tree` as it stands now.
    pub fn capture(name: impl Into<String>, tree: &LayerTree) -> Self {
        let layers = tree
            .iter_depth_first()
            .into_iter()
            .filter_map(|id| tree.get(id))
            .map(|l| CompLayerState {
                layer: l.id,
                visible: l.visible,
                transform: l.transform.to_cols_array(),
                opacity: l.opacity,
                fill_opacity: l.fill_opacity,
                blend_mode: l.blend_mode,
                effects: l.effects.clone(),
            })
            .collect();
        Self {
            name: name.into(),
            comment: String::new(),
            layers,
            flags: CompFlags::default(),
        }
    }

    /// The recorded state of `layer`, if the comp knows it.
    pub fn state_of(&self, layer: LayerId) -> Option<&CompLayerState> {
        self.layers.iter().find(|s| s.layer == layer)
    }
}

/// A note pinned to the document.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Note {
    /// Stable within the document ([`DocumentExtras::next_id`]).
    pub id: u64,
    /// Where the pin sits, in document pixels.
    pub x: f32,
    pub y: f32,
    pub author: String,
    pub text: String,
}

/// A named character style: the family, the size and the base style of a
/// text run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CharacterStyle {
    pub id: u64,
    pub name: String,
    pub font_family: String,
    pub size_px: f32,
    pub style: BaseStyle,
}

impl Default for CharacterStyle {
    fn default() -> Self {
        let text = TextLayer::default();
        Self {
            id: 0,
            name: String::new(),
            font_family: text.font_family,
            size_px: text.size_px,
            style: text.style,
        }
    }
}

impl CharacterStyle {
    /// The style a text layer's run currently wears.
    pub fn from_text(id: u64, name: impl Into<String>, text: &TextLayer) -> Self {
        Self {
            id,
            name: name.into(),
            font_family: text.font_family.clone(),
            size_px: text.size_px,
            style: text.style,
        }
    }

    /// Put this style on a text layer's run: family, size and base style.
    /// The text itself, its per-range spans and its paragraph settings are
    /// untouched.
    pub fn apply_to(&self, text: &mut TextLayer) {
        text.font_family = self.font_family.clone();
        text.size_px = self.size_px;
        text.style = self.style;
    }

    /// `kind` with this style applied, or `None` when `kind` is not text.
    pub fn applied(&self, kind: &LayerKind) -> Option<LayerKind> {
        let LayerKind::Text(text) = kind else {
            return None;
        };
        let mut text = text.clone();
        self.apply_to(&mut text);
        Some(LayerKind::Text(text))
    }
}

/// A named paragraph style.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ParagraphStyle {
    pub id: u64,
    pub name: String,
    pub paragraph: Paragraph,
}

impl ParagraphStyle {
    /// The paragraph settings a text layer currently wears.
    pub fn from_text(id: u64, name: impl Into<String>, text: &TextLayer) -> Self {
        Self {
            id,
            name: name.into(),
            paragraph: text.paragraph,
        }
    }

    /// Put this style on a text layer. The layer's orientation (vertical
    /// type) is the layer's own, not the style's, and is kept.
    pub fn apply_to(&self, text: &mut TextLayer) {
        let vertical = text.paragraph.vertical;
        text.paragraph = self.paragraph;
        text.paragraph.vertical = vertical;
    }

    /// `kind` with this style applied, or `None` when `kind` is not text.
    pub fn applied(&self, kind: &LayerKind) -> Option<LayerKind> {
        let LayerKind::Text(text) = kind else {
            return None;
        };
        let mut text = text.clone();
        self.apply_to(&mut text);
        Some(LayerKind::Text(text))
    }
}

/// Which styles one text layer wears.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StyleLink {
    pub layer: LayerId,
    #[serde(default)]
    pub character: Option<u64>,
    #[serde(default)]
    pub paragraph: Option<u64>,
}

/// A saved selection open as an editable alpha channel: the index of the
/// saved selection and the hidden scratch layer whose mask carries its
/// coverage while it is edited.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct AlphaEdit {
    pub index: usize,
    pub layer: LayerId,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layer::Layer;

    #[test]
    fn an_empty_record_round_trips_and_an_old_document_loads_empty() {
        let empty = DocumentExtras::default();
        assert!(empty.is_empty());
        let back: DocumentExtras = serde_json::from_str("{}").unwrap();
        assert_eq!(back, empty);
        let mut full = DocumentExtras::default();
        full.notes.push(Note {
            id: 1,
            x: 3.0,
            y: 4.0,
            author: "A".into(),
            text: "check the edge".into(),
        });
        let json = serde_json::to_string(&full).unwrap();
        assert_eq!(serde_json::from_str::<DocumentExtras>(&json).unwrap(), full);
    }

    #[test]
    fn a_comp_captures_every_layer() {
        let mut tree = LayerTree::new();
        let a = tree.push_root(Layer::raster("A")).unwrap();
        let mut hidden = Layer::raster("B");
        hidden.visible = false;
        let b = tree.push_root(hidden).unwrap();
        let comp = LayerComp::capture("One", &tree);
        assert_eq!(comp.layers.len(), 2);
        assert!(comp.state_of(a).unwrap().visible);
        assert!(!comp.state_of(b).unwrap().visible);
    }

    #[test]
    fn links_are_kept_per_layer_and_pruned_when_empty() {
        let mut x = DocumentExtras::default();
        let layer = LayerId::new();
        x.link_paragraph(layer, Some(7));
        x.link_character(layer, Some(8));
        assert_eq!(x.layers_with_paragraph(7), vec![layer]);
        assert_eq!(x.layers_with_character(8), vec![layer]);
        x.link_paragraph(layer, None);
        x.link_character(layer, None);
        assert!(x.style_links.is_empty());
    }

    #[test]
    fn a_paragraph_style_keeps_the_layers_orientation() {
        let mut text = TextLayer::default();
        text.paragraph.vertical = true;
        let mut style = ParagraphStyle::default();
        style.paragraph.first_line_indent = 12.0;
        style.apply_to(&mut text);
        assert!(text.paragraph.vertical);
        assert_eq!(text.paragraph.first_line_indent, 12.0);
    }
}

#[cfg(test)]
mod w16e_tests {
    use super::*;

    /// W16-E: a comp saved before the flags existed loads with all three
    /// on (it applies as it always did), and a record without the Last
    /// Document State loads with none.
    #[test]
    fn a_comp_written_before_the_flags_loads_with_every_flag_on() {
        let old = r#"{"layer_comps":[{"name":"A","comment":"","layers":[]}],"last_comp":0}"#;
        let x: DocumentExtras = serde_json::from_str(old).unwrap();
        assert_eq!(x.layer_comps[0].flags, CompFlags::default());
        assert!(x.layer_comps[0].flags.visibility);
        assert!(x.layer_comps[0].flags.position);
        assert!(x.layer_comps[0].flags.appearance);
        assert!(x.last_document_state.is_none());
    }

    #[test]
    fn cleared_flags_and_the_last_state_survive_a_round_trip() {
        let mut x = DocumentExtras::default();
        let mut comp = LayerComp::capture("Birds", &LayerTree::new());
        comp.flags.position = false;
        comp.flags.appearance = false;
        x.layer_comps.push(comp.clone());
        x.last_document_state = Some(LayerComp::capture("Last", &LayerTree::new()));
        let json = serde_json::to_string(&x).unwrap();
        let back: DocumentExtras = serde_json::from_str(&json).unwrap();
        assert_eq!(back, x);
        assert_eq!(
            back.layer_comps[0].flags,
            CompFlags {
                visibility: true,
                position: false,
                appearance: false
            }
        );
    }
}
