//! W16-N: the English strings the UI draws that are not catalogue rows —
//! labels the model crates own, translated by their text where they are
//! drawn ([`super::tr_en`]) — gathered for the language-table gate.

use crate::dock::{LayoutId, PanelId};
use crate::menu::{Entry, MenuAction};
use crate::panels::text as text_panel;

/// Proper names every table keeps as they are: formats, colour standards,
/// product names. The gate does not ask a language to translate them.
pub(super) const PROPER_NAMES: &[&str] = &[
    "RGB",
    "CMYK",
    "Lab",
    "HSB",
    "HSL",
    "Hex",
    "CSS",
    "SVG",
    "PNG",
    "JPG",
    "JPEG",
    "GIF",
    "WebP",
    "BMP",
    "TIFF",
    "PSD",
    "PDF",
    "ICO",
    "TGA",
    "EMF",
    "DXF",
    "HEIC",
    "AVIF",
    "MP4",
    "RAW",
    "EXIF",
    "XMP",
    "ICC",
    "sRGB",
    "Adobe RGB (1998)",
    "Display P3",
    "ProPhoto RGB",
    "sRGB IEC61966-2.1",
    "Raster Studio",
    "X",
    "Y",
];

/// Every English string drawn from outside the catalogue. Called under the
/// English locale ([`super::catalogue_sources`]), so the menu model hands
/// back its sources rather than their translations.
pub(super) fn sources() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // The menus: titles, submenus and every item, menu-listed or not.
    fn walk(entries: &[Entry], out: &mut Vec<String>) {
        for entry in entries {
            match entry {
                Entry::Item(action) => out.push(action.label()),
                Entry::Separator => {}
                Entry::Submenu { label, entries } => {
                    out.push((*label).to_string());
                    walk(entries, out);
                }
            }
        }
    }
    for menu in crate::menu::menu_bar(1) {
        out.push(menu.title.to_string());
        walk(&menu.entries, &mut out);
    }
    out.extend(MenuAction::all().into_iter().map(MenuAction::label));
    // A language is named in itself, and a recent-file slot by its number.
    let own_names: Vec<String> = crate::strings::Locale::ALL
        .iter()
        .map(|l| MenuAction::SetLanguage(*l).label())
        .chain((0..crate::menu::MAX_RECENT_FILES).map(|i| MenuAction::OpenRecent(i).label()))
        .collect();
    out.retain(|s| !own_names.contains(s));
    out.extend(
        ["Undo", "Redo", "Layer", crate::menu::MERGE_LAYERS]
            .iter()
            .map(|s| s.to_string()),
    );
    // Panel tabs and Window > Workspace.
    out.extend(PanelId::ALL.iter().map(|p| p.english_title().to_string()));
    out.extend(LayoutId::ALL.iter().map(|l| l.title().to_string()));
    // Blend modes (Layers panel, smart-filter rows).
    out.extend(
        layer_model::BlendMode::ALL
            .iter()
            .map(|m| m.label().to_string()),
    );
    // History steps.
    out.extend(
        editor_core::command::HISTORY_LABELS
            .iter()
            .map(|s| s.to_string()),
    );
    // The Adjustments panel's buttons.
    out.extend(
        crate::menu::AdjustmentId::ALL
            .iter()
            .map(|a| a.label().to_string()),
    );
    // The Character and Paragraph panels' choices.
    out.extend(text_panel::WEIGHTS.iter().map(|(n, _)| n.to_string()));
    out.extend(
        text_panel::SCRIPTS
            .iter()
            .map(|s| text_panel::script_label(*s).to_string()),
    );
    out.extend(
        text_panel::CAPS
            .iter()
            .map(|c| text_panel::caps_label(*c).to_string()),
    );
    out.extend(
        text_panel::ALIGNMENTS
            .iter()
            .map(|a| text_panel::alignment_label(*a).to_string()),
    );
    out.extend(
        text_panel::ANTI_ALIAS
            .iter()
            .map(|a| text_panel::anti_alias_label(*a).to_string()),
    );
    out.extend(
        text_panel::KerningMode::ALL
            .iter()
            .map(|k| k.label().to_string()),
    );
    out.extend(
        [
            "Ultra Condensed",
            "Extra Condensed",
            "Condensed",
            "Semi Condensed",
            "Semi Expanded",
            "Expanded",
            "Extra Expanded",
            "Ultra Expanded",
            "Italic",
            "Oblique",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    // The Info panel's rows.
    out.extend(
        ["Pointer", "Document", "Selection", "Distance", "Angle"]
            .iter()
            .map(|s| s.to_string()),
    );
    // The Properties panel's headings (`PropertiesSubject::english_title`)
    // and the Color panel's notation names (`ColorNotation::label`).
    out.extend(
        [
            "Properties",
            "Layer Properties",
            "Mask Properties",
            "Adjustment",
            "Text Properties",
            "Shape Properties",
            "Smart Object",
            "Fill Layer",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    out.extend(
        crate::panels::color::ColorNotation::ALL
            .iter()
            .map(|n| n.label().to_string()),
    );
    // The Brushes panel's shipped presets.
    out.extend(
        crate::panels::brushes::BrushesState::default()
            .presets()
            .iter()
            .map(|p| p.name.clone()),
    );
    out
}
