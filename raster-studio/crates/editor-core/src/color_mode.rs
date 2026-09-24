//! Document colour modes (Image > Mode) and the one-step conversion command.
//!
//! Tiles are always stored RGBA — the Photopea approach: one RGB working
//! buffer, and the colour mode decides how the pixels are *constrained* and
//! how they are *read out* (Info, export). A conversion is therefore two
//! things in ONE [`Command::Transaction`], so one undo restores both:
//!
//! * [`Command::SetMetaColorMode`] — the document's mode flag;
//! * one [`Command::PaintTiles`] per pixel-bearing layer whose pixels the
//!   target mode constrains (Grayscale collapses to luma, CMYK clamps to the
//!   printable set, Indexed maps onto the palette). RGB and Lab constrain
//!   nothing: 8-bit sRGB round-trips through Lab, so those conversions change
//!   the flag alone.
//!
//! The pixel mapping itself belongs to the caller (it needs the tile store,
//! which this crate never holds); [`convert_color_mode`] walks the document,
//! hands every level-0 tile to it, and assembles the command.

use layer_model::LayerId;
use raster::{TileCoord, TileHash};

use crate::command::{Command, CommandError};
use crate::document::Document;
use crate::pixels::{PixelTarget, TileEdit};

/// [`crate::DocumentMeta::color_mode`] values, in `ui::menu::ColorMode`
/// discriminant order.
pub mod mode {
    pub const RGB: u8 = 0;
    pub const GRAYSCALE: u8 = 1;
    pub const LAB: u8 = 2;
    pub const CMYK: u8 = 3;
    pub const INDEXED: u8 = 4;
    /// W10-H: black and white only (from Grayscale; one flattened layer
    /// whose every pixel is `0` or `255`, opaque — 1-bit semantics in 8-bit
    /// tiles).
    pub const BITMAP: u8 = 5;
    /// W10-H: a grayscale image printed through one to four inks, baked
    /// into the tiles at conversion (`color::duotone`).
    pub const DUOTONE: u8 = 6;
}

/// Whether `mode` is a colour mode this build knows.
pub const fn is_known_color_mode(mode: u8) -> bool {
    mode <= mode::DUOTONE
}

/// Whether converting INTO `mode` rewrites pixels (as opposed to only the
/// flag). RGB and Lab do not: both keep every 8-bit sRGB colour.
pub const fn constrains_pixels(mode: u8) -> bool {
    matches!(
        mode,
        mode::GRAYSCALE | mode::CMYK | mode::INDEXED | mode::BITMAP | mode::DUOTONE
    )
}

/// W10-H: whether a document in `from` can be converted into `to` directly.
/// Photoshop's rule: Bitmap and Duotone are reached from Grayscale only
/// (and a Bitmap or Duotone document leaves to Grayscale or RGB). Every
/// other pair is allowed.
pub const fn conversion_allowed(from: u8, to: u8) -> bool {
    match to {
        mode::BITMAP | mode::DUOTONE => from == mode::GRAYSCALE,
        _ => true,
    }
}

/// Build the one-step conversion of `doc` into colour mode `to`.
///
/// `rewrite` receives every level-0 tile of every layer (its layer, coord and
/// current hash) and returns the hash of the converted tile, or `None` to
/// leave it as it is. It is only consulted when [`constrains_pixels`]`(to)`.
///
/// Refuses an unknown mode ([`CommandError::UnsupportedColorMode`]) and a
/// conversion into the mode the document already has
/// ([`CommandError::ColorModeUnchanged`]).
pub fn convert_color_mode(
    doc: &Document,
    to: u8,
    label: impl Into<String>,
    mut rewrite: impl FnMut(LayerId, TileCoord, TileHash) -> Option<TileHash>,
) -> Result<Command, CommandError> {
    if !is_known_color_mode(to) {
        return Err(CommandError::UnsupportedColorMode(to));
    }
    let from = doc.meta.color_mode;
    if from == to {
        return Err(CommandError::ColorModeUnchanged(to));
    }
    let mut commands = vec![Command::SetMetaColorMode { from, to }];
    if constrains_pixels(to) {
        for layer_id in doc.layers.iter_depth_first() {
            let Some(map) = doc.layer_tiles(layer_id) else {
                continue;
            };
            let edits: Vec<TileEdit> = map
                .iter()
                .filter(|(coord, _)| coord.level == 0)
                .filter_map(|(coord, hash)| {
                    rewrite(layer_id, coord, hash)
                        .filter(|new| *new != hash)
                        .map(|new| TileEdit::set(coord, new))
                })
                .collect();
            if !edits.is_empty() {
                commands.push(Command::paint_tiles(PixelTarget::Layer(layer_id), edits)?);
            }
        }
    }
    Ok(Command::Transaction {
        label: label.into(),
        commands,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::History;
    use layer_model::Layer;

    fn hash(n: u8) -> TileHash {
        TileHash::of(&[n])
    }

    fn doc_with_a_tile() -> (Document, LayerId) {
        let mut doc = Document::new(8, 8, "modes");
        let layer = Layer::raster("px");
        let id = layer.id;
        Command::create_layer(layer).apply(&mut doc).unwrap();
        Command::paint_tiles(
            PixelTarget::Layer(id),
            [TileEdit::set(TileCoord::new(0, 0, 0), hash(1))],
        )
        .unwrap()
        .apply(&mut doc)
        .unwrap();
        (doc, id)
    }

    #[test]
    fn a_pixel_mode_conversion_is_one_undo_step_for_flag_and_pixels() {
        let (mut doc, id) = doc_with_a_tile();
        let mut history = History::default();
        let before = doc.layer_tiles(id).unwrap().clone();
        let cmd = convert_color_mode(&doc, mode::CMYK, "to CMYK", |_, _, _| Some(hash(2))).unwrap();
        history.apply(&mut doc, cmd).unwrap();
        assert_eq!(doc.meta.color_mode, mode::CMYK);
        assert_ne!(doc.layer_tiles(id).unwrap(), &before);
        assert_eq!(history.journal().count(), 1, "one step");
        history.undo(&mut doc).unwrap();
        assert_eq!(doc.meta.color_mode, mode::RGB);
        assert_eq!(doc.layer_tiles(id).unwrap(), &before);
    }

    #[test]
    fn lab_changes_the_flag_alone_and_consults_no_tile() {
        let (doc, id) = doc_with_a_tile();
        let before = doc.layer_tiles(id).unwrap().clone();
        let cmd = convert_color_mode(&doc, mode::LAB, "to Lab", |_, _, _| {
            panic!("Lab must not rewrite pixels")
        })
        .unwrap();
        let mut doc = doc;
        cmd.apply(&mut doc).unwrap();
        assert_eq!(doc.meta.color_mode, mode::LAB);
        assert_eq!(doc.layer_tiles(id).unwrap(), &before);
    }

    #[test]
    fn an_unknown_or_unchanged_mode_is_refused() {
        let (doc, _) = doc_with_a_tile();
        assert!(matches!(
            convert_color_mode(&doc, 9, "?", |_, _, _| None),
            Err(CommandError::UnsupportedColorMode(9))
        ));
        assert!(matches!(
            convert_color_mode(&doc, mode::RGB, "?", |_, _, _| None),
            Err(CommandError::ColorModeUnchanged(mode::RGB))
        ));
    }

    /// W10-H: Bitmap and Duotone are known modes that rewrite pixels, and
    /// only a Grayscale document converts into them.
    #[test]
    fn bitmap_and_duotone_are_known_pixel_modes_reached_from_grayscale() {
        for m in [mode::BITMAP, mode::DUOTONE] {
            assert!(is_known_color_mode(m));
            assert!(constrains_pixels(m));
            assert!(conversion_allowed(mode::GRAYSCALE, m));
            assert!(!conversion_allowed(mode::RGB, m));
            assert!(conversion_allowed(m, mode::RGB));
        }
        assert!(!is_known_color_mode(mode::DUOTONE + 1));
        let mut doc = Document::new(4, 4, "j");
        Command::SetMetaColorMode {
            from: 0,
            to: mode::DUOTONE,
        }
        .apply(&mut doc)
        .unwrap();
        assert_eq!(doc.meta.color_mode, mode::DUOTONE);
    }

    #[test]
    fn a_journal_cannot_set_an_unknown_mode() {
        let mut doc = Document::new(4, 4, "j");
        assert!(matches!(
            Command::SetMetaColorMode { from: 0, to: 7 }.apply(&mut doc),
            Err(CommandError::UnsupportedColorMode(7))
        ));
        assert_eq!(doc.meta.color_mode, 0);
    }
}
