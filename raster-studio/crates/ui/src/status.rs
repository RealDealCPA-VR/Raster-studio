//! The status bar: what the document is, what the tool wants, what is running.
//!
//! Every field is derived, and every one formats through a function here, so
//! the bar and the panels that show the same number cannot disagree about how
//! it is written. The zoom field is editable, which makes it the one place in
//! the bar that produces an intent — [`StatusBar::parse_zoom`] is
//! [`crate::panels::navigator::parse_zoom`], so typing `200%` into the status
//! bar and into the Navigator do the same thing.

use editor_core::Document;
use raster::TILE_SIZE;
use tools::ToolId;

use crate::intent::Progress;
use crate::panels::navigator::{format_zoom, parse_zoom};

/// Bytes one resident tile costs: RGBA8 at the tile store's tile size.
const BYTES_PER_TILE: u64 = (TILE_SIZE as u64) * (TILE_SIZE as u64) * 4;

/// One readout of the status bar.
#[derive(Clone, PartialEq, Debug)]
pub struct StatusField {
    pub label: &'static str,
    pub value: String,
}

/// The status bar's derived content.
#[derive(Clone, PartialEq, Debug)]
pub struct StatusBar {
    pub zoom: f32,
    pub tool: Option<ToolId>,
    pub progress: Option<Progress>,
}

impl Default for StatusBar {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            tool: None,
            progress: None,
        }
    }
}

impl StatusBar {
    pub fn new() -> Self {
        Self::default()
    }

    /// The fields, in bar order.
    pub fn fields(&self, doc: &Document) -> Vec<StatusField> {
        vec![
            StatusField {
                label: "Zoom",
                value: format_zoom(self.zoom),
            },
            StatusField {
                label: "Size",
                value: format_dimensions(doc),
            },
            StatusField {
                label: "Mode",
                value: doc.meta.color_space.name().to_string(),
            },
            StatusField {
                label: "Memory",
                value: format_bytes(estimated_bytes(doc)),
            },
        ]
    }

    /// The hint shown for the active tool: its name, and the letter that
    /// selects it.
    ///
    /// Not a description of what a drag does — the registry publishes no such
    /// sentence, and inventing one here would be a second place for it to be
    /// wrong.
    pub fn tool_hint(&self) -> String {
        let Some(tool) = self.tool else {
            return "No tool".to_string();
        };
        match crate::palette::info(tool) {
            Some(info) => match info.shortcut {
                Some(key) => format!("{}  ({})", info.name, key.to_ascii_uppercase()),
                None => info.name.to_string(),
            },
            None => "No tool".to_string(),
        }
    }

    /// Parse a zoom the user typed into the field.
    pub fn parse_zoom(text: &str) -> Option<f32> {
        parse_zoom(text)
    }

    /// `true` when a long operation is running, so the bar shows a progress
    /// indicator in place of the memory readout.
    pub fn is_busy(&self) -> bool {
        self.progress.is_some()
    }
}

/// `1920 × 1080 px`, the one spelling of a document's size.
pub fn format_dimensions(doc: &Document) -> String {
    format!("{} × {} px", doc.width(), doc.height())
}

/// How much pixel memory the document's resident tiles account for.
///
/// This is the *document's* footprint — one RGBA8 tile per stored tile
/// reference — and it deliberately does not try to be the process's resident
/// set: the number a user acts on is "how big is what I am editing", and a
/// figure that moved with the allocator would be unreadable. Deduplicated
/// tiles are counted once each, which is what the tile store actually holds.
pub fn estimated_bytes(doc: &Document) -> u64 {
    doc.pixels.tile_count() as u64 * BYTES_PER_TILE
}

/// Bytes in the unit a person reads, with one decimal above a kilobyte.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::{Command, History, PixelTarget, TileEdit};
    use layer_model::Layer;
    use raster::{TileCoord, TileHash};

    #[test]
    fn the_bar_reports_zoom_size_mode_and_memory() {
        let doc = Document::new(1920, 1080, "Shoot");
        let bar = StatusBar::new();
        let fields = bar.fields(&doc);
        assert_eq!(fields.len(), 4);
        assert_eq!(fields[0].value, "100%");
        assert_eq!(fields[1].value, "1920 × 1080 px");
        assert_eq!(fields[2].value, "sRGB");
        assert_eq!(fields[3].value, "0 B");
        assert!(fields.iter().all(|f| !f.label.is_empty()));
    }

    #[test]
    fn the_zoom_field_shows_and_parses_the_same_spelling() {
        let doc = Document::new(64, 64, "Test");
        let bar = StatusBar {
            zoom: 0.25,
            ..StatusBar::new()
        };
        let shown = bar.fields(&doc)[0].value.clone();
        assert_eq!(shown, "25%");
        assert_eq!(StatusBar::parse_zoom(&shown), Some(0.25));
        assert_eq!(StatusBar::parse_zoom("nonsense"), None);
    }

    #[test]
    fn memory_climbs_as_tiles_are_stored() {
        let mut doc = Document::new(256, 256, "Test");
        let layer = doc.layers.push_root(Layer::raster("Paint")).unwrap();
        assert_eq!(estimated_bytes(&doc), 0);

        let mut history = History::new();
        history
            .apply(
                &mut doc,
                Command::paint_tiles(
                    PixelTarget::Layer(layer),
                    [
                        TileEdit::set(TileCoord::new(0, 0, 0), TileHash::of(&[1])),
                        TileEdit::set(TileCoord::new(1, 0, 0), TileHash::of(&[2])),
                    ],
                )
                .expect("a well-formed delta"),
            )
            .expect("apply");
        assert_eq!(estimated_bytes(&doc), 2 * BYTES_PER_TILE);
        assert_eq!(
            StatusBar::new().fields(&doc)[3].value,
            format_bytes(2 * BYTES_PER_TILE)
        );

        // ...and undo gives it back.
        history.undo(&mut doc).unwrap();
        assert_eq!(estimated_bytes(&doc), 0);
    }

    #[test]
    fn bytes_are_written_in_the_unit_a_person_reads() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn a_gigantic_figure_still_lands_on_a_unit_rather_than_running_off_the_end() {
        let huge = format_bytes(u64::MAX);
        assert!(huge.ends_with(" TB"), "{huge}");
    }

    #[test]
    fn the_tool_hint_names_the_tool_and_its_key() {
        let bar = StatusBar {
            tool: Some(ToolId::Brush),
            ..StatusBar::new()
        };
        let hint = bar.tool_hint();
        assert!(hint.contains("Brush"), "{hint}");
        assert!(hint.contains('B'), "{hint}");
    }

    #[test]
    fn with_no_tool_the_hint_says_so_rather_than_being_blank() {
        assert_eq!(StatusBar::new().tool_hint(), "No tool");
    }

    #[test]
    fn every_tool_produces_a_non_empty_hint() {
        for tool in ToolId::ALL {
            let bar = StatusBar {
                tool: Some(*tool),
                ..StatusBar::new()
            };
            assert!(!bar.tool_hint().is_empty(), "{tool:?}");
        }
    }

    #[test]
    fn the_bar_knows_when_something_is_running() {
        let mut bar = StatusBar::new();
        assert!(!bar.is_busy());
        bar.progress = Some(Progress::new("Applying Gaussian Blur", 0.4));
        assert!(bar.is_busy());
        assert_eq!(bar.progress.as_ref().unwrap().fraction, Some(0.4));
    }
}
