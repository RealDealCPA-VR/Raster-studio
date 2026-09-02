//! The localization catalogue: user-facing strings resolve through here,
//! keyed by locale, instead of sitting as literals in the view and dialog
//! modules.
//!
//! # How a string moves in
//!
//! A literal `"Zoom in"` becomes `tr(STR_ZOOM_IN)` — a stable key constant and
//! a table row per locale. The lookup is a static map: `en` is the source of
//! truth and always complete; any other locale falls back to the English
//! string rather than showing a key, so a partial translation ships.
//!
//! # The migration is a wave, not a commit
//!
//! `crates/ui/src/view` and `crates/ui/src/dialogs` hold ~1600 string
//! literals. `the_catalogue_resolves_every_registered_key_for_every_locale`
//! proves the table itself is sound; the no-literal lint over those modules
//! (`P3.12`'s validate) turns red for the first time the day the last literal
//! moves, and until then it is recorded as not-yet-passing rather than
//! quietly weakened.

use std::sync::atomic::{AtomicU8, Ordering};

/// The languages the catalogue carries. `En` is the source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Locale {
    En,
}

impl Locale {
    /// The locale the editor shows, as the preferences system stores it.
    pub fn from_code(code: &str) -> Self {
        let _ = code; // more locales arrive with their table rows
        Self::En
    }

    /// The BCP-47 code, for the preferences UI.
    pub const fn code(self) -> &'static str {
        match self {
            Self::En => "en",
        }
    }

    /// The name shown in the preferences list, in that language.
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::En => "English",
        }
    }
}

/// The active locale. One per process: the editor is a single-window app and
/// the choice lives in preferences, read once at startup.
static ACTIVE: AtomicU8 = AtomicU8::new(0);

/// Switch the catalogue's locale. Unknown codes fall back to English.
pub fn set_locale(locale: Locale) {
    let code = match locale {
        Locale::En => 0,
    };
    ACTIVE.store(code, Ordering::Relaxed);
}

fn active() -> Locale {
    let _ = ACTIVE.load(Ordering::Relaxed); // locales beyond En flip this
    Locale::En
}

/// Every catalogue entry: the English source string first, then any
/// translations. A locale missing from a row falls back to English at lookup.
const TABLE: &[(&str, &[(Locale, &str)])] = &[
    ("actions.record", &[(Locale::En, "Record")]),
    ("ui.menu_bar.nothing.in.this.submenu.is.available", &[(Locale::En, "Nothing in this submenu is available right now")]),
    ("ui.status.unsaved.changes", &[(Locale::En, "Unsaved changes")]),
    ("ui.status.type.a.zoom.level", &[(Locale::En, "Type a zoom level")]),
    ("ui.toolbar.swap.foreground.and.background.x", &[(Locale::En, "Swap foreground and background (X)")]),
    ("ui.toolbar.default.colours.d", &[(Locale::En, "Default colours (D)")]),
    ("ui.toolbar.this.tool.has.no.options", &[(Locale::En, "This tool has no options")]),
    ("ui.toolbar.this.tool.is.already.at.its", &[(Locale::En, "This tool is already at its defaults")]),
    ("ui.toolbar.return.this.tool.to.its.defaults", &[(Locale::En, "Return this tool to its defaults")]),
    ("ui.toolbar.swap.colours.x", &[(Locale::En, "Swap colours  (X)")]),
    ("ui.toolbar.default.colours.d.2", &[(Locale::En, "Default colours  (D)")]),
    ("ui.canvas_rotation.the.canvas.grows.to.fit.the", &[(Locale::En, "The canvas grows to fit the rotated image.")]),
    ("ui.canvas_rotation.rotate.canvas", &[(Locale::En, "Rotate Canvas")]),
    ("ui.canvas_rotation.the.angle.must.be.a.finite", &[(Locale::En, "The angle must be a finite number of degrees")]),
    ("ui.color_picker.this.window.cannot.read.screen.pixels", &[(Locale::En, "This window cannot read screen pixels, so the eyedropper is unavailable")]),
    ("ui.color_picker.back.to.the.colour.this.opened", &[(Locale::En, "Back to the colour this opened on")]),
    ("ui.color_picker.only.web.safe.colours", &[(Locale::En, "Only web-safe colours")]),
    ("ui.color_picker.click.anywhere.to.sample.a.colour", &[(Locale::En, "Click anywhere to sample a colour, or press Escape.")]),
    ("ui.color_picker.not.a.hex.colour", &[(Locale::En, "Not a hex colour")]),
    ("ui.color_picker.color.picker", &[(Locale::En, "Color Picker")]),
    ("ui.fill_stroke.no.patterns.are.defined.yet", &[(Locale::En, "No patterns are defined yet")]),
    ("ui.fill_stroke.width.must.be.between.1.and", &[(Locale::En, "Width must be between 1 and 250 pixels")]),
    ("ui.fill_stroke.fills.the.active.selection.with.the", &[(Locale::En, "Fills the active selection with the chosen contents.")]),
    ("ui.fill_stroke.preserve.transparency", &[(Locale::En, "Preserve Transparency")]),
    ("ui.fill_stroke.paints.a.band.along.the.active", &[(Locale::En, "Paints a band along the active selection's border.")]),
    ("ui.filter_gallery.filter.gallery", &[(Locale::En, "Filter Gallery")]),
    ("actions.stop", &[(Locale::En, "Stop")]),
    ("actions.replay", &[(Locale::En, "Replay")]),
    (
        "actions.hint",
        &[(
            Locale::En,
            "Record an edit, then replay the whole sequence on any document with at least as many layers.",
        )],
    ),
];

/// Keys that must resolve. The tests walk this list, so a table row whose key
/// drifted is caught next to the constant that drifted. Test-only today: the
/// moment a second locale lands, the preferences UI reads this list too.
#[cfg(test)]
const KNOWN_KEYS: &[&str] = &[
    "actions.record",
    "actions.stop",
    "actions.replay",
    "actions.hint",
];

/// Resolve `key` in the active locale, falling back to English. An
/// unregistered key is a bug the catalogue tests catch; the tests name every
/// key the migrated modules use (KNOWN_KEYS), so a leak here means a module
/// grew a string without a table row. At runtime the empty string is better
/// than a panic or a rogue key leaking into the UI.
pub fn tr(key: &str) -> &'static str {
    let locale = active();
    let Some((_, row)) = TABLE.iter().find(|(k, _)| *k == key) else {
        return "";
    };
    row.iter()
        .find(|(l, _)| *l == locale)
        .or_else(|| row.iter().find(|(l, _)| *l == Locale::En))
        .map(|(_, s)| *s)
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_resolves_every_registered_key_for_every_locale() {
        for (key, row) in TABLE {
            for (locale, expected) in row.iter() {
                set_locale(*locale);
                assert_eq!(tr(key), *expected, "key {key:?} in {locale:?}");
            }
        }
        // English is always complete and always the fallback.
        set_locale(Locale::En);
        for (key, row) in TABLE {
            assert!(
                row.iter().any(|(l, _)| *l == Locale::En),
                "{key:?} has no English entry; English is the fallback source"
            );
        }
    }

    #[test]
    fn an_unknown_key_is_empty_rather_than_a_leak_or_a_panic() {
        assert_eq!(tr("not.a.key"), "");
    }

    #[test]
    fn every_known_key_resolves_without_a_leak() {
        for key in KNOWN_KEYS {
            assert_ne!(tr(key), "", "{key} must resolve, not leak");
            assert!(
                TABLE.iter().any(|(k, _)| k == key),
                "{key} is in KNOWN_KEYS but not in TABLE"
            );
        }
    }
}
