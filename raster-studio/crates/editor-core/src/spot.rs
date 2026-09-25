//! W13X-4: the document's spot channels — Photoshop's / Photopea's Channels
//! ▸ New Spot Channel.
//!
//! A spot channel is a plate of one premixed ink: a name, the ink's colour
//! as it looks on paper, a **solidity** (0% prints as a transparent ink that
//! darkens what is under it, 100% as an opaque ink that covers it) and a
//! coverage — how much ink each pixel carries. It is not a layer: the
//! compositor lays every spot channel over the composited image as ink
//! (`compositor::composite`'s spot pass), in list order, after the layer
//! stack.
//!
//! The record rides on the [`Document`] (saved with the `.rstudio` document,
//! omitted while there is none) and changes through
//! [`Command::SetSpotChannels`], so creating one is a single undo step.
//! Coverage reuses [`Selection`]: New Spot Channel with a selection active
//! fills the channel with that selection (Photoshop's behaviour); with none
//! the channel starts empty.

use glam::IVec2;
use serde::{Deserialize, Serialize};

use crate::command::Command;
use crate::document::Document;
use crate::selection::Selection;

/// Photoshop's default solidity for a new spot channel.
pub const DEFAULT_SOLIDITY: u8 = 0;

/// One spot channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpotChannel {
    pub name: String,
    /// The ink as it looks printed, 8-bit sRGB.
    pub ink: [u8; 3],
    /// 0..=100: 0 is a transparent ink (it multiplies), 100 an opaque one
    /// (it covers).
    pub solidity: u8,
    /// How much ink each document pixel carries. [`Selection::None`] would
    /// mean "everywhere", so an empty channel is stored as an empty rect.
    pub coverage: Selection,
}

impl SpotChannel {
    /// A channel with no ink anywhere.
    pub fn empty(name: impl Into<String>, ink: [u8; 3], solidity: u8) -> Self {
        Self {
            name: name.into(),
            ink,
            solidity: solidity.min(100),
            coverage: Selection::Rect {
                min: IVec2::ZERO,
                max: IVec2::ZERO,
            },
        }
    }

    /// How much ink the pixel at `p` carries, `0.0..=1.0`.
    pub fn ink_at(&self, p: IVec2) -> f32 {
        self.coverage.coverage_at(p)
    }

    /// Solidity as a fraction.
    pub fn solidity_fraction(&self) -> f32 {
        f32::from(self.solidity.min(100)) / 100.0
    }
}

/// The next free default name: "Spot Color 1", "Spot Color 2", ...
pub fn next_spot_name(doc: &Document) -> String {
    (1..)
        .map(|n| format!("Spot Color {n}"))
        .find(|name| doc.spot_channels.iter().all(|c| &c.name != name))
        .unwrap_or_default()
}

/// Channels ▸ New Spot Channel: the command that appends a channel named
/// `name` in `ink` at `solidity`, covering the document's current selection
/// (or nothing, with no selection). One undo step.
pub fn new_spot_channel(doc: &Document, name: &str, ink: [u8; 3], solidity: u8) -> Command {
    let mut channel = SpotChannel::empty(name, ink, solidity);
    if !doc.selection.is_none() {
        channel.coverage = doc.selection.clone();
    }
    let mut channels = doc.spot_channels.clone();
    channels.push(channel);
    Command::SetSpotChannels { channels }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::History;

    #[test]
    fn a_new_spot_channel_takes_the_selection_and_undoes_in_one_step() {
        let mut doc = Document::new(8, 8, "spot");
        doc.selection = Selection::Rect {
            min: IVec2::new(2, 2),
            max: IVec2::new(4, 4),
        };
        let before = doc.clone();
        let mut history = History::new();
        let name = next_spot_name(&doc);
        assert_eq!(name, "Spot Color 1");
        let command = new_spot_channel(&doc, &name, [0, 160, 80], 40);
        history.apply(&mut doc, command).unwrap();
        assert_eq!(doc.spot_channels.len(), 1);
        let c = &doc.spot_channels[0];
        assert_eq!((c.ink, c.solidity), ([0, 160, 80], 40));
        assert_eq!(c.ink_at(IVec2::new(3, 3)), 1.0);
        assert_eq!(c.ink_at(IVec2::new(5, 5)), 0.0);
        assert_eq!(next_spot_name(&doc), "Spot Color 2");
        history.undo(&mut doc).unwrap();
        assert_eq!(doc, before);
    }

    #[test]
    fn with_no_selection_the_channel_starts_empty_not_full() {
        let doc = Document::new(8, 8, "spot");
        let Command::SetSpotChannels { channels } =
            new_spot_channel(&doc, "Gold", [200, 160, 0], 0)
        else {
            panic!("not a spot-channel command");
        };
        assert_eq!(channels[0].ink_at(IVec2::new(0, 0)), 0.0);
        assert!(channels[0].coverage.is_empty());
    }

    #[test]
    fn spot_channels_survive_the_serialized_document_and_are_omitted_while_empty() {
        let mut doc = Document::new(8, 8, "spot");
        let empty = serde_json::to_string(&doc).unwrap();
        assert!(!empty.contains("spot_channels"), "{empty}");
        doc.spot_channels.push(SpotChannel {
            coverage: Selection::Rect {
                min: IVec2::new(1, 1),
                max: IVec2::new(3, 3),
            },
            ..SpotChannel::empty("Pantone 485", [218, 41, 28], 100)
        });
        let json = serde_json::to_string(&doc).unwrap();
        let back: Document = serde_json::from_str(&json).unwrap();
        assert_eq!(back.spot_channels, doc.spot_channels);
        assert_eq!(back, doc);
        // A document written before spot channels existed still opens.
        let old: Document = serde_json::from_str(&empty).unwrap();
        assert!(old.spot_channels.is_empty());
    }
}
