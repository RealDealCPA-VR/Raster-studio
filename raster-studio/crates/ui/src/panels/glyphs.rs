//! W10-B: the Glyphs panel.
//!
//! A grid of the characters the active font has — punctuation, accented
//! letters, currency, typographic quotes and dashes, arrows and a few maths
//! signs — drawn *in that font*, and a click on a cell inserts the character
//! into the text being edited. The panel raises [`Intent::InsertGlyph`] for
//! the active text layer and the application decides where it lands: at the
//! caret of the live typing session when one is open (into the session's
//! draft, so the character is part of the run the session confirms), and
//! otherwise at the end of the layer's committed text as one undo step
//! ([`insert_glyph`]).
//!
//! # Which font, which characters
//!
//! The font is the active text layer's family (the document default when no
//! text layer is active). Each candidate code point in [`CANDIDATES`] is
//! shaped through the text engine's shared font library, and a cell is kept
//! only when the shaper answered it from the font's *own* primary face with a
//! real glyph (not `.notdef`, not a fallback face) — so the grid is the
//! font's repertoire, not the system's.
//!
//! The kept glyphs are rasterised once per family into one atlas texture
//! (white coverage, tinted with the theme's text colour when painted) and
//! cached in egui's memory; a cell draws its slice of the atlas. egui's own
//! font never draws a cell, so no cell can come out as a tofu box.
//!
//! When the library cannot shape at all (a machine with no fonts), the grid
//! falls back to printable ASCII drawn with the UI font, which egui has.
//!
//! # Strings
//!
//! Through the strings catalogue ([`crate::strings::tr`]): each constant
//! below is a catalogue key.

use std::sync::Arc;

use design::{color32, current_tokens, egui_theme::rounding, ColorRole, Radius, Space, TextRole};
use editor_core::{Command, Document};
use egui::{Sense, Ui, Vec2};
use layer_model::{LayerId, LayerKind};

use crate::intent::Intent;
use crate::strings::tr;
use crate::view::{empty_state, hint, text};
use crate::Workspace;

const NO_DOCUMENT: &str = "ui.glyphs.no_document";
const NO_TEXT: &str = "ui.glyphs.no_text";
const FONT: &str = "ui.glyphs.font";
const DEFAULT_FONT: &str = "ui.glyphs.default_font";

/// The code points offered, in grid order.
pub const CANDIDATES: &[(u32, u32)] = &[
    (0x21, 0x7E),
    (0xA1, 0xAC),
    (0xAE, 0xFF),
    (0x2013, 0x2014),
    (0x2018, 0x201E),
    (0x2020, 0x2022),
    (0x2026, 0x2026),
    (0x2030, 0x2030),
    (0x2039, 0x203A),
    (0x20AC, 0x20AC),
    (0x2122, 0x2122),
    (0x2190, 0x2195),
    (0x2212, 0x2212),
    (0x221E, 0x221E),
    (0x2248, 0x2248),
    (0x2260, 0x2260),
    (0x2264, 0x2265),
];

/// Every candidate character, in grid order.
pub fn candidates() -> Vec<char> {
    CANDIDATES
        .iter()
        .flat_map(|(lo, hi)| (*lo..=*hi).filter_map(char::from_u32))
        .collect()
}

/// The pixel size glyphs are rasterised at for the atlas.
const ATLAS_PX: f32 = 24.0;
/// The atlas cell edge, in texels: the raster size plus a margin.
const CELL_TEXELS: usize = 32;
const ATLAS_COLUMNS: usize = 16;

/// Stable ids for a headless test.
pub mod ids {
    /// The cell of character `c`.
    pub fn cell(c: char) -> egui::Id {
        egui::Id::new(("raster-glyphs-cell", c as u32))
    }
}

/// One family's glyph grid: the characters it has and, when the text engine
/// could rasterise them, the atlas they are drawn from.
#[derive(Clone)]
pub struct GlyphSet {
    pub chars: Vec<char>,
    /// The atlas texture; cell `i` of `chars` is at column `i % 16`, row
    /// `i / 16`. `None` in the ASCII fallback.
    atlas: Option<egui::TextureHandle>,
    rows: usize,
}

/// The characters of `family` among [`CANDIDATES`], each with its coverage
/// raster (`CELL_TEXELS` square, glyph centred). `None` when the library
/// cannot shape anything (no fonts).
pub fn family_repertoire(family: &str) -> Option<Vec<(char, Vec<u8>)>> {
    text_engine::with_shared_library(|library| {
        let mut cache = text_engine::GlyphRasterCache::new();
        let probe =
            text_engine::shape(library, &text_engine::TextRun::point("A", family, ATLAS_PX));
        let primary = probe.glyphs.first().filter(|g| g.glyph_id != 0)?.font;
        let mut out = Vec::new();
        for c in candidates() {
            let shaped = text_engine::shape(
                library,
                &text_engine::TextRun::point(c.to_string(), family, ATLAS_PX),
            );
            let own = shaped.glyphs.len() == 1
                && shaped.glyphs[0].glyph_id != 0
                && shaped.glyphs[0].font == primary;
            if !own {
                continue;
            }
            let mask = text_engine::rasterize(library, &mut cache, &shaped);
            out.push((c, centred_cell(&mask)));
        }
        Some(out)
    })
}

/// `mask` centred in a `CELL_TEXELS` square, cropped if larger.
fn centred_cell(mask: &text_engine::CoverageMask) -> Vec<u8> {
    let mut cell = vec![0u8; CELL_TEXELS * CELL_TEXELS];
    let (w, h) = (mask.width as usize, mask.height as usize);
    let ox = (CELL_TEXELS as isize - w as isize) / 2;
    let oy = (CELL_TEXELS as isize - h as isize) / 2;
    for y in 0..h {
        for x in 0..w {
            let (cx, cy) = (x as isize + ox, y as isize + oy);
            if cx < 0 || cy < 0 || cx >= CELL_TEXELS as isize || cy >= CELL_TEXELS as isize {
                continue;
            }
            cell[cy as usize * CELL_TEXELS + cx as usize] = mask.data[y * w + x];
        }
    }
    cell
}

fn set_key(family: &str) -> egui::Id {
    egui::Id::new(("raster-glyphs-set", family.to_string()))
}

/// The grid for `family`, built once and kept in egui's memory.
pub fn glyph_set(ctx: &egui::Context, family: &str) -> Arc<GlyphSet> {
    if let Some(set) = ctx.data(|d| d.get_temp::<Arc<GlyphSet>>(set_key(family))) {
        return set;
    }
    let set = match family_repertoire(family).filter(|r| !r.is_empty()) {
        Some(repertoire) => {
            let rows = repertoire.len().div_ceil(ATLAS_COLUMNS);
            let (aw, ah) = (ATLAS_COLUMNS * CELL_TEXELS, rows * CELL_TEXELS);
            let mut rgba = vec![0u8; aw * ah * 4];
            for (i, (_, cell)) in repertoire.iter().enumerate() {
                let (col, row) = (i % ATLAS_COLUMNS, i / ATLAS_COLUMNS);
                for y in 0..CELL_TEXELS {
                    for x in 0..CELL_TEXELS {
                        let px = (row * CELL_TEXELS + y) * aw + col * CELL_TEXELS + x;
                        let a = cell[y * CELL_TEXELS + x];
                        rgba[px * 4..px * 4 + 4].copy_from_slice(&[u8::MAX, u8::MAX, u8::MAX, a]);
                    }
                }
            }
            let image = egui::ColorImage::from_rgba_unmultiplied([aw, ah], &rgba);
            let atlas = ctx.load_texture(
                format!("raster-glyphs-{family}"),
                image,
                egui::TextureOptions::LINEAR,
            );
            GlyphSet {
                chars: repertoire.into_iter().map(|(c, _)| c).collect(),
                atlas: Some(atlas),
                rows,
            }
        }
        None => {
            let chars: Vec<char> = (0x21u8..=0x7E).map(char::from).collect();
            let rows = chars.len().div_ceil(ATLAS_COLUMNS);
            GlyphSet {
                chars,
                atlas: None,
                rows,
            }
        }
    };
    let set = Arc::new(set);
    ctx.data_mut(|d| d.insert_temp(set_key(family), Arc::clone(&set)));
    set
}

/// The active layer when it is an editable text layer.
fn active_text(doc: &Document) -> Option<(LayerId, &layer_model::TextLayer)> {
    let id = doc.active_layer()?;
    let layer = doc.layers.get(id)?;
    if layer.locked.all {
        return None;
    }
    match &layer.kind {
        LayerKind::Text(text) => Some((id, text)),
        _ => None,
    }
}

/// The command that appends `glyph` to text layer `layer`'s committed text —
/// the application's route when no typing session is open.
pub fn insert_glyph(doc: &Document, layer: LayerId, glyph: &str) -> Option<Command> {
    let layer_ref = doc.layers.get(layer)?;
    if layer_ref.locked.all {
        return None;
    }
    let LayerKind::Text(text) = &layer_ref.kind else {
        return None;
    };
    let mut text = text.clone();
    text.text.push_str(glyph);
    Some(Command::SetLayerKind {
        layer_id: layer,
        kind: Box::new(LayerKind::Text(text)),
    })
}

/// Draw the panel.
pub(crate) fn glyphs_body(w: &mut Workspace, ui: &mut Ui, doc: &Document) {
    if doc.width() == 0 || doc.height() == 0 {
        empty_state(ui, tr(NO_DOCUMENT));
        return;
    }
    let active = active_text(doc);
    let family = active
        .map(|(_, t)| t.font_family.clone())
        .unwrap_or_default();
    let shown = if family.is_empty() {
        tr(DEFAULT_FONT)
    } else {
        family.as_str()
    };
    ui.label(text(
        ui,
        format!("{}{shown}", tr(FONT)),
        TextRole::Secondary,
        design::TypeRole::Caption,
    ));
    if active.is_none() {
        ui.label(hint(ui, tr(NO_TEXT)));
    }
    let set = glyph_set(ui.ctx(), &family);
    let t = current_tokens(ui);
    let side = t.metrics.list_row_height * 1.25;
    let columns = ((ui.available_width() / side).floor() as usize).max(1);
    let mut picked: Option<char> = None;
    egui::ScrollArea::vertical()
        .id_salt("raster-glyphs-grid")
        .max_height(side * 8.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            for row in set.chars.chunks(columns) {
                ui.horizontal(|ui| {
                    for c in row {
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(side), Sense::hover());
                        let response = ui.interact(rect, ids::cell(*c), Sense::click());
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                active.is_some(),
                                format!("U+{:04X}", *c as u32),
                            )
                        });
                        if ui.is_rect_visible(rect) {
                            paint_cell(ui, &set, *c, rect, response.hovered());
                        }
                        if response.clicked() {
                            picked = Some(*c);
                        }
                    }
                });
            }
        });
    if let (Some(c), Some((layer, _))) = (picked, active) {
        w.emit(Intent::InsertGlyph {
            layer,
            text: c.to_string(),
        });
    }
}

fn paint_cell(ui: &Ui, set: &GlyphSet, c: char, rect: egui::Rect, hovered: bool) {
    let t = current_tokens(ui);
    let radius = Radius::Small.resolve(&t.radii, rect.height());
    if hovered {
        ui.painter().rect_filled(
            rect,
            rounding(radius),
            color32(t.palette.color(ColorRole::ControlFillHovered)),
        );
    }
    let ink = color32(t.palette.text(TextRole::Primary));
    match (&set.atlas, set.chars.iter().position(|x| *x == c)) {
        (Some(atlas), Some(i)) => {
            let (col, row) = ((i % ATLAS_COLUMNS) as f32, (i / ATLAS_COLUMNS) as f32);
            let (cols, rows) = (ATLAS_COLUMNS as f32, set.rows.max(1) as f32);
            let uv = egui::Rect::from_min_max(
                egui::pos2(col / cols, row / rows),
                egui::pos2((col + 1.0) / cols, (row + 1.0) / rows),
            );
            let inner = rect.shrink(Space::Hair.pt());
            let edge = inner.width().min(inner.height());
            let target = egui::Rect::from_center_size(rect.center(), Vec2::splat(edge));
            ui.painter().image(atlas.id(), target, uv, ink);
        }
        _ => {
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                c,
                design::egui_theme::font_id(t, design::TypeRole::Body),
                ink,
            );
        }
    }
    ui.painter().rect_stroke(
        rect,
        rounding(radius),
        egui::Stroke::new(
            t.borders.hairline,
            color32(t.palette.color(ColorRole::ControlStroke)),
        ),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W10-B: every string the panel shows is a catalogue key that resolves.
    #[test]
    fn every_string_resolves_through_the_catalogue() {
        for key in [NO_DOCUMENT, NO_TEXT, FONT, DEFAULT_FONT] {
            assert!(!tr(key).is_empty(), "{key} is not in the catalogue");
        }
    }

    #[test]
    fn the_candidates_hold_ascii_and_typographic_marks() {
        let all = candidates();
        assert!(all.contains(&'A'));
        assert!(all.contains(&char::from_u32(0x2014).unwrap()));
        assert!(!all.contains(&' '), "a space is not a glyph to pick");
        // The soft hyphen is invisible; it is left out on purpose.
        assert!(!all.contains(&char::from_u32(0xAD).unwrap()));
    }

    #[test]
    fn a_glyph_appends_to_the_text_layer() {
        let mut doc = Document::new(8, 8, "t");
        let layer = layer_model::Layer::with_kind(
            "T",
            LayerKind::Text(layer_model::TextLayer {
                text: "ab".into(),
                ..Default::default()
            }),
        );
        let id = layer.id;
        Command::create_layer(layer).apply(&mut doc).unwrap();
        let insert = insert_glyph(&doc, id, "c").unwrap();
        insert.apply(&mut doc).unwrap();
        let LayerKind::Text(t) = &doc.layers.get(id).unwrap().kind else {
            panic!()
        };
        assert_eq!(t.text, "abc");
    }
}
