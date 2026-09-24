//! W11-E: the Layers panel's colour labels (Photoshop's and Photopea's
//! "No Color / Red / Orange / Yellow / Green / Blue / Violet / Gray").
//!
//! A label is organisation, not appearance: nothing composites it. It is kept
//! in the document's [`crate::DocumentExtras`] (one [`LayerColorLabel`] row per
//! labelled layer) rather than on [`crate::Layer`], so setting one is a
//! `SetDocumentExtras` edit — one undo step — and a document written before
//! labels existed loads with none.

use serde::{Deserialize, Serialize};

use crate::doc_extras::DocumentExtras;
use crate::ids::LayerId;

/// One of the eight labels. Appended only (serde): a new colour goes at the
/// end.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, Serialize, Deserialize)]
pub enum ColorLabel {
    /// No label: the row shows no chip.
    #[default]
    NoColor,
    Red,
    Orange,
    Yellow,
    Green,
    Blue,
    Violet,
    Gray,
}

impl ColorLabel {
    /// Every label, in the order the Layers panel menu lists them.
    pub const ALL: &'static [ColorLabel] = &[
        ColorLabel::NoColor,
        ColorLabel::Red,
        ColorLabel::Orange,
        ColorLabel::Yellow,
        ColorLabel::Green,
        ColorLabel::Blue,
        ColorLabel::Violet,
        ColorLabel::Gray,
    ];

    /// The row name the menus show.
    pub const fn label(self) -> &'static str {
        match self {
            ColorLabel::NoColor => "No Color",
            ColorLabel::Red => "Red",
            ColorLabel::Orange => "Orange",
            ColorLabel::Yellow => "Yellow",
            ColorLabel::Green => "Green",
            ColorLabel::Blue => "Blue",
            ColorLabel::Violet => "Violet",
            ColorLabel::Gray => "Gray",
        }
    }

    /// The colour the label names, as 8-bit sRGB — document data (the
    /// user's choice of tag), not a design-system colour. `None` for
    /// [`ColorLabel::NoColor`]. Photoshop's label swatches.
    pub const fn rgb(self) -> Option<[u8; 3]> {
        Some(match self {
            ColorLabel::NoColor => return None,
            ColorLabel::Red => [232, 76, 61],
            ColorLabel::Orange => [242, 146, 51],
            ColorLabel::Yellow => [241, 205, 64],
            ColorLabel::Green => [98, 179, 88],
            ColorLabel::Blue => [74, 144, 226],
            ColorLabel::Violet => [155, 107, 208],
            ColorLabel::Gray => [150, 150, 150],
        })
    }

    /// Photoshop's `lclr` sheet-colour index for this label (0 none, 1 red,
    /// 2 orange, 3 yellow, 4 green, 5 blue, 6 violet, 7 gray).
    pub const fn psd_index(self) -> u16 {
        match self {
            ColorLabel::NoColor => 0,
            ColorLabel::Red => 1,
            ColorLabel::Orange => 2,
            ColorLabel::Yellow => 3,
            ColorLabel::Green => 4,
            ColorLabel::Blue => 5,
            ColorLabel::Violet => 6,
            ColorLabel::Gray => 7,
        }
    }

    /// The label a PSD `lclr` index names; an index this build does not know
    /// reads as no label.
    pub const fn from_psd_index(index: u16) -> Self {
        match index {
            1 => ColorLabel::Red,
            2 => ColorLabel::Orange,
            3 => ColorLabel::Yellow,
            4 => ColorLabel::Green,
            5 => ColorLabel::Blue,
            6 => ColorLabel::Violet,
            7 => ColorLabel::Gray,
            _ => ColorLabel::NoColor,
        }
    }
}

/// One labelled layer.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct LayerColorLabel {
    pub layer: LayerId,
    pub color: ColorLabel,
}

impl DocumentExtras {
    /// The label `layer` wears ([`ColorLabel::NoColor`] when none).
    pub fn color_label(&self, layer: LayerId) -> ColorLabel {
        self.layer_colors
            .iter()
            .find(|l| l.layer == layer)
            .map_or(ColorLabel::NoColor, |l| l.color)
    }

    /// Give `layer` the label `color`; [`ColorLabel::NoColor`] removes its
    /// row, so an unlabelled document stores nothing.
    pub fn set_color_label(&mut self, layer: LayerId, color: ColorLabel) {
        self.layer_colors.retain(|l| l.layer != layer);
        if color != ColorLabel::NoColor {
            self.layer_colors.push(LayerColorLabel { layer, color });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_label_is_set_replaced_cleared_and_round_trips() {
        let a = LayerId::new();
        let b = LayerId::new();
        let mut extras = DocumentExtras::default();
        assert_eq!(extras.color_label(a), ColorLabel::NoColor);
        extras.set_color_label(a, ColorLabel::Red);
        extras.set_color_label(b, ColorLabel::Blue);
        extras.set_color_label(a, ColorLabel::Green);
        assert_eq!(extras.color_label(a), ColorLabel::Green);
        assert_eq!(extras.color_label(b), ColorLabel::Blue);
        assert_eq!(extras.layer_colors.len(), 2, "one row per layer");
        let json = serde_json::to_string(&extras).unwrap();
        let back: DocumentExtras = serde_json::from_str(&json).unwrap();
        assert_eq!(back, extras);
        extras.set_color_label(a, ColorLabel::NoColor);
        extras.set_color_label(b, ColorLabel::NoColor);
        assert!(extras.is_empty(), "no label stores nothing");
    }

    #[test]
    fn every_label_maps_to_its_psd_index_and_back() {
        for (i, c) in ColorLabel::ALL.iter().enumerate() {
            assert_eq!(c.psd_index() as usize, i);
            assert_eq!(ColorLabel::from_psd_index(c.psd_index()), *c);
            assert_eq!(c.rgb().is_none(), *c == ColorLabel::NoColor);
        }
        assert_eq!(ColorLabel::from_psd_index(99), ColorLabel::NoColor);
    }
}
