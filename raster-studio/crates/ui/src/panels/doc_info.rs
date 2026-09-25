//! W13-N: the Document Info panel (Photopea's Window ▸ Document Info).
//!
//! A read-only card of facts about the active document: its size in pixels
//! and at the document resolution, the colour mode and bit depth, the colour
//! profile, how many layers it holds, and how much memory its pixels take
//! uncompressed. [`DocInfo::of`] computes every row from the document alone,
//! so the panel, a test and anything else that wants the numbers read the
//! same answer.
//!
//! # Memory
//!
//! The pixel store is content-addressed: identical tiles are stored once.
//! The figure is the number of *distinct* tiles in use (layers, masks and
//! every other pixel plane) times the size of one uncompressed tile at the
//! document's bit depth. It is not the process's resident size — the
//! compositor's caches and the undo history are not the document's.

use design::{Space, TextRole, TypeRole};
use editor_core::Document;
use egui::Ui;

use crate::menu::ColorMode;
use crate::strings::tr;
use crate::view::{empty_state, hint, text};
use crate::Workspace;

/// Stable ids for a headless test.
pub mod ids {
    /// The value cell of row `index` in [`super::DocInfo::rows`] order.
    pub fn row(index: usize) -> egui::Id {
        egui::Id::new(("raster-doc-info-row", index))
    }
}

/// Every fact the panel shows.
#[derive(Clone, Debug, PartialEq)]
pub struct DocInfo {
    pub width: u32,
    pub height: u32,
    /// Pixels per inch, as the rulers and Image Size read it.
    pub ppi: f32,
    pub mode: ColorMode,
    /// Bits per channel: 8, 16 or 32.
    pub depth: u8,
    /// The colour profile's name.
    pub profile: String,
    /// Every layer in the tree, groups and their children included.
    pub layers: usize,
    /// Distinct pixel tiles in use.
    pub tiles: usize,
    /// `tiles` uncompressed, in bytes.
    pub memory_bytes: u64,
}

impl DocInfo {
    /// The facts of `doc` at `ppi`.
    pub fn of(doc: &Document, ppi: f32) -> Self {
        let mut hashes = std::collections::HashSet::new();
        for key in doc.pixels.keys() {
            if let Some(tiles) = doc.pixels.tiles(key) {
                for (_, hash) in tiles.iter() {
                    hashes.insert(hash);
                }
            }
        }
        let depth = match doc.meta.bit_depth {
            16 => 16,
            32 => 32,
            _ => 8,
        };
        let tile_side = u64::from(raster::TILE_SIZE);
        let bytes_per_pixel = 4 * u64::from(depth) / 8;
        let profile = match &doc.meta.color_space {
            color::ColorSpace::IccProfile { asset_hash, .. } => {
                let short: String = asset_hash.chars().take(8).collect();
                format!("{} {short}", doc.meta.color_space.name())
            }
            other => other.name().to_string(),
        };
        Self {
            width: doc.width(),
            height: doc.height(),
            ppi: if ppi.is_finite() && ppi > 0.0 {
                ppi
            } else {
                72.0
            },
            mode: ColorMode::from_meta(doc.meta.color_mode),
            depth,
            profile,
            layers: doc.layers.len(),
            tiles: hashes.len(),
            memory_bytes: hashes.len() as u64 * tile_side * tile_side * bytes_per_pixel,
        }
    }

    /// The panel's rows, label key then value, top to bottom.
    pub fn rows(&self) -> Vec<(&'static str, String)> {
        let inches = |px: u32| f64::from(px) / f64::from(self.ppi);
        vec![
            (SIZE, format!("{} x {} px", self.width, self.height)),
            (
                PRINT_SIZE,
                format!("{:.2} x {:.2} in", inches(self.width), inches(self.height)),
            ),
            (RESOLUTION, format!("{} ppi", trim(self.ppi))),
            (
                MODE,
                format!(
                    "{}, {} bits/channel",
                    self.mode.label().trim_end_matches('…'),
                    self.depth
                ),
            ),
            (PROFILE, self.profile.clone()),
            (LAYERS, self.layers.to_string()),
            (
                MEMORY,
                format!("{} ({} tiles)", megabytes(self.memory_bytes), self.tiles),
            ),
        ]
    }
}

/// A number without a trailing `.0`.
fn trim(v: f32) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Bytes as `x.y MB` (binary megabytes, as Photopea shows them).
pub fn megabytes(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
}

const NO_DOCUMENT: &str = "ui.doc_info.no_document";
const SIZE: &str = "ui.doc_info.size";
const PRINT_SIZE: &str = "ui.doc_info.print_size";
const RESOLUTION: &str = "ui.doc_info.resolution";
const MODE: &str = "ui.doc_info.mode";
const PROFILE: &str = "ui.doc_info.profile";
const LAYERS: &str = "ui.doc_info.layers";
const MEMORY: &str = "ui.doc_info.memory";

/// Draw the panel.
pub(crate) fn doc_info_body(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    if doc.width() == 0 || doc.height() == 0 {
        empty_state(ui, tr(NO_DOCUMENT));
        return;
    }
    let info = DocInfo::of(doc, w.canvas.resolution_ppi);
    egui::Grid::new("raster-doc-info-grid")
        .num_columns(2)
        .spacing(egui::vec2(Space::Small.pt(), Space::XSmall.pt()))
        .show(ui, |ui| {
            for (index, (key, value)) in info.rows().into_iter().enumerate() {
                ui.label(hint(ui, tr(key)));
                let cell = ui.label(text(ui, value, TextRole::Primary, TypeRole::Footnote));
                let _ = ui.interact(cell.rect, ids::row(index), egui::Sense::hover());
                ui.end_row();
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use layer_model::Layer;

    #[test]
    fn every_string_resolves_through_the_catalogue() {
        for key in [
            NO_DOCUMENT,
            SIZE,
            PRINT_SIZE,
            RESOLUTION,
            MODE,
            PROFILE,
            LAYERS,
            MEMORY,
        ] {
            assert!(!tr(key).is_empty(), "{key} is not in the catalogue");
        }
    }

    #[test]
    fn the_facts_come_from_the_document() {
        let mut doc = Document::new(600, 300, "info");
        doc.meta.bit_depth = 16;
        doc.meta.color_mode = 1;
        for name in ["A", "B"] {
            editor_core::Command::create_layer(Layer::raster(name))
                .apply(&mut doc)
                .unwrap();
        }
        let info = DocInfo::of(&doc, 300.0);
        assert_eq!((info.width, info.height), (600, 300));
        assert_eq!(info.mode, ColorMode::Grayscale);
        assert_eq!(info.depth, 16);
        assert_eq!(info.layers, 2);
        assert_eq!(info.profile, "sRGB");
        // No pixels were painted: nothing is stored.
        assert_eq!((info.tiles, info.memory_bytes), (0, 0));
        let rows = info.rows();
        assert_eq!(rows[0].1, "600 x 300 px");
        assert_eq!(rows[1].1, "2.00 x 1.00 in");
        assert_eq!(rows[2].1, "300 ppi");
        assert_eq!(rows[3].1, "Grayscale, 16 bits/channel");
    }
}
