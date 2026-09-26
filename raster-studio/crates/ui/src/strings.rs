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

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

/// The languages the catalogue carries. `En` is the source of truth.
///
/// W16-N: Photopea's More > Language list, cut to the languages this build
/// translates in full — every English string the catalogue knows has a row in
/// each of their tables (`crates/ui/src/i18n/<code>.tsv`), which
/// `every_language_table_translates_every_catalogue_string` enforces. The rest
/// of Photopea's list is not offered: a language picker that switched to a
/// half-English UI would promise what the table cannot show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Locale {
    #[default]
    En,
    De,
    Es,
    Fr,
    It,
    Pl,
    PtBr,
    Tr,
    Ru,
    Uk,
    ZhCn,
    Ja,
    Ko,
}

impl Locale {
    /// Every locale the catalogue has a table for, in preferences-list order
    /// (English, then the Latin-script languages, Cyrillic, CJK).
    pub const ALL: &'static [Locale] = &[
        Locale::En,
        Locale::De,
        Locale::Es,
        Locale::Fr,
        Locale::It,
        Locale::Pl,
        Locale::PtBr,
        Locale::Tr,
        Locale::Ru,
        Locale::Uk,
        Locale::ZhCn,
        Locale::Ja,
        Locale::Ko,
    ];

    /// The locale the editor shows, as the preferences system stores it.
    /// Unknown codes fall back to English.
    pub fn from_code(code: &str) -> Self {
        Self::ALL
            .iter()
            .copied()
            .find(|l| l.code().eq_ignore_ascii_case(code))
            .unwrap_or(Self::En)
    }

    /// The BCP-47 code, for the preferences file.
    pub const fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::De => "de",
            Self::Es => "es",
            Self::Fr => "fr",
            Self::It => "it",
            Self::Pl => "pl",
            Self::PtBr => "pt-BR",
            Self::Tr => "tr",
            Self::Ru => "ru",
            Self::Uk => "uk",
            Self::ZhCn => "zh-CN",
            Self::Ja => "ja",
            Self::Ko => "ko",
        }
    }

    /// The name shown in the language list, in that language (the table's
    /// `@name` row: a native name like the Japanese one is not ASCII, and the
    /// Rust sources stay ASCII by gate).
    pub fn display_name(self) -> &'static str {
        match catalogue(self) {
            Some(c) => c.name,
            None => "English",
        }
    }

    /// Whether this language's script needs the bundled CJK face
    /// ([`install_fonts`]); egui's own font covers Latin and Cyrillic.
    pub const fn needs_cjk_font(self) -> bool {
        matches!(self, Self::ZhCn | Self::Ja | Self::Ko)
    }

    /// The translation table, one `English<TAB>translation` row per line.
    const fn source(self) -> Option<&'static str> {
        Some(match self {
            Self::En => return None,
            Self::De => include_str!("i18n/de.tsv"),
            Self::Es => include_str!("i18n/es.tsv"),
            Self::Fr => include_str!("i18n/fr.tsv"),
            Self::It => include_str!("i18n/it.tsv"),
            Self::Pl => include_str!("i18n/pl.tsv"),
            Self::PtBr => include_str!("i18n/pt-BR.tsv"),
            Self::Tr => include_str!("i18n/tr.tsv"),
            Self::Ru => include_str!("i18n/ru.tsv"),
            Self::Uk => include_str!("i18n/uk.tsv"),
            Self::ZhCn => include_str!("i18n/zh-CN.tsv"),
            Self::Ja => include_str!("i18n/ja.tsv"),
            Self::Ko => include_str!("i18n/ko.tsv"),
        })
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|l| *l == self).unwrap_or(0)
    }
}

/// The active locale. One per process: the editor is a single-window app and
/// the choice lives in preferences, installed at startup and again the moment
/// the preference changes (`Editor::set_preferences`).
static ACTIVE: AtomicU8 = AtomicU8::new(0);

thread_local! {
    /// A locale pinned for the current thread by [`with_locale`], ahead of the
    /// process-wide one. Tests run in parallel threads of one process: a test
    /// that switched the process-wide locale would flip every other test's
    /// strings under it.
    static SCOPED: Cell<Option<Locale>> = const { Cell::new(None) };
}

/// Switch the catalogue's locale for the whole process.
pub fn set_locale(locale: Locale) {
    ACTIVE.store(locale.index() as u8, Ordering::Relaxed);
}

/// The locale in force on this thread.
pub fn active() -> Locale {
    if let Some(scoped) = SCOPED.with(Cell::get) {
        return scoped;
    }
    let index = ACTIVE.load(Ordering::Relaxed) as usize;
    Locale::ALL.get(index).copied().unwrap_or(Locale::En)
}

/// Run `f` with `locale` in force on this thread only, restoring what was
/// there before even if `f` panics.
pub fn with_locale<R>(locale: Locale, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<Locale>);
    impl Drop for Restore {
        fn drop(&mut self) {
            SCOPED.with(|s| s.set(self.0));
        }
    }
    let _restore = Restore(SCOPED.with(|s| s.replace(Some(locale))));
    f()
}

/// One language's parsed table: its native name and English-to-translation
/// rows.
struct Catalogue {
    name: &'static str,
    rows: HashMap<&'static str, &'static str>,
}

/// The parsed table for `locale`, built once on first use.
fn catalogue(locale: Locale) -> Option<&'static Catalogue> {
    static CATALOGUES: [OnceLock<Catalogue>; 13] = [const { OnceLock::new() }; 13];
    let source = locale.source()?;
    Some(CATALOGUES[locale.index()].get_or_init(|| parse_catalogue(source)))
}

/// Parse one `.tsv` table. Blank lines and `#` comments are skipped; `@name`
/// names the language; every other line is `English<TAB>translation`, with
/// `\n`, `\t` and `\\` escapes in either column.
fn parse_catalogue(source: &'static str) -> Catalogue {
    let mut name = "";
    let mut rows = HashMap::new();
    for line in source.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((english, translated)) = line.split_once('\t') else {
            continue;
        };
        if english == "@name" {
            name = translated;
            continue;
        }
        rows.insert(unescape(english), unescape(translated));
    }
    Catalogue { name, rows }
}

/// A table cell with its escapes resolved. Rows without a backslash borrow
/// the embedded table; the few that carry one are built once and kept.
fn unescape(cell: &'static str) -> &'static str {
    if !cell.contains('\\') {
        return cell;
    }
    let mut out = String::with_capacity(cell.len());
    let mut chars = cell.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    Box::leak(out.into_boxed_str())
}

/// W16-N: `english` in the active locale — the menu labels, panel titles,
/// blend-mode names and history steps are English source strings the model
/// crates own (`layer_model::BlendMode::label` is a `const fn` the tool
/// options fold at compile time), so they are translated by their text where
/// they are drawn. A string the table has no row for is shown as it is: a
/// file name or a layer the user named is never mangled.
pub fn tr_en(english: &str) -> &str {
    let locale = active();
    if locale == Locale::En {
        return english;
    }
    let Some(catalogue) = catalogue(locale) else {
        return english;
    };
    if let Some(row) = catalogue.rows.get(english) {
        return row;
    }
    // A history step is named after the menu row that made it, without the
    // row's ellipsis ("Gaussian Blur" from the "Gaussian Blur" row, ellipsis dropped).
    if !english.is_empty() && !english.ends_with('\u{2026}') {
        let with_ellipsis = format!("{english}\u{2026}");
        if let Some(row) = catalogue.rows.get(with_ellipsis.as_str()) {
            return row.strip_suffix('\u{2026}').unwrap_or(row);
        }
    }
    english
}

/// [`tr_en`] for an owned label, such as `MenuAction::label`'s.
pub fn tr_owned(english: String) -> String {
    match tr_en(&english) {
        same if same == english => english,
        translated => translated.to_string(),
    }
}

/// The id [`install_fonts`] marks a context with once its fonts are in.
fn fonts_installed_id() -> egui::Id {
    egui::Id::new("raster-i18n-fonts-installed")
}

/// W16-N: add the bundled CJK face (a subset of Noto Sans CJK SC, SIL OFL 1.1:
/// `i18n/OFL.txt`) to egui's fallback chain, after egui's own fonts, so the
/// Chinese, Japanese and Korean tables — and those languages' names in the
/// language list — draw as glyphs rather than empty boxes. Idempotent per
/// context: the chrome calls it with every theme install.
///
/// The face is cut to the characters the three tables use (plus kana and
/// CJK punctuation); arbitrary CJK text a user types into a dialog field can
/// still meet a glyph the subset does not carry.
pub fn install_fonts(ctx: &egui::Context) {
    let id = fonts_installed_id();
    if ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false) {
        return;
    }
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        CJK_FONT_NAME.to_string(),
        egui::FontData::from_static(CJK_FONT),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push(CJK_FONT_NAME.to_string());
    }
    ctx.set_fonts(fonts);
    ctx.data_mut(|d| d.insert_temp(id, true));
}

/// The name the CJK face is registered under in egui's font definitions.
pub const CJK_FONT_NAME: &str = "raster-noto-sans-cjk-subset";

/// The bundled CJK face (see [`install_fonts`]).
const CJK_FONT: &[u8] = include_bytes!("i18n/NotoSansCJKsc-subset.otf");
/// Every catalogue entry: the English source string first, then any
/// translations. A locale missing from a row falls back to English at lookup.
const TABLE: &[(&str, &[(Locale, &str)])] = &[
    // W13X-7: File > Open's PDF import dialog (pages, resolution, mode).
    ("ui.pdf_import.title", &[(Locale::En, "Import PDF")]),
    ("ui.pdf_import.subtitle", &[(Locale::En, "Choose the pages to open, their resolution and how they open.")]),
    ("ui.pdf_import.file", &[(Locale::En, "{name}: {n} pages")]),
    ("ui.pdf_import.listed", &[(Locale::En, "The first {shown} of {n} pages are listed; a longer file opens only those.")]),
    ("ui.pdf_import.pages", &[(Locale::En, "Pages")]),
    ("ui.pdf_import.page", &[(Locale::En, "Page {n}")]),
    ("ui.pdf_import.resolution", &[(Locale::En, "Resolution")]),
    ("ui.pdf_import.dpi", &[(Locale::En, "dpi")]),
    ("ui.pdf_import.size", &[(Locale::En, "Page {page} opens at {w} x {h} px")]),
    ("ui.pdf_import.open_as", &[(Locale::En, "Open as")]),
    ("ui.pdf_import.mode.artboards", &[(Locale::En, "Artboards in one document")]),
    ("ui.pdf_import.mode.separate", &[(Locale::En, "Separate documents")]),
    ("ui.pdf_import.none", &[(Locale::En, "Choose at least one page to open")]),
    ("ui.pdf_import.dpi_range", &[(Locale::En, "The resolution must be {min} to {max} dpi")]),
    ("ui.pdf_import.ok", &[(Locale::En, "Open")]),
    ("ui.pdf_import.all", &[(Locale::En, "Select All")]),
    ("ui.pdf_import.none_button", &[(Locale::En, "Select None")]),
    // W13-F: Assign / Convert to Profile, Reduce Colors, Wavelet Decompose,
    // Pattern Preview and the slice rows (menu, dialogs, status lines).
    ("ui.w13f.profile.srgb", &[(Locale::En, "sRGB IEC61966-2.1")]),
    ("ui.w13f.profile.adobe_rgb", &[(Locale::En, "Adobe RGB (1998)")]),
    ("ui.w13f.profile.display_p3", &[(Locale::En, "Display P3")]),
    ("ui.w13f.profile.prophoto", &[(Locale::En, "ProPhoto RGB")]),
    ("ui.w13f.profile.from_file", &[(Locale::En, "Profile from File…")]),
    ("ui.w13f.menu.assign_profile", &[(Locale::En, "Assign Profile")]),
    ("ui.w13f.menu.convert_to_profile", &[(Locale::En, "Convert to Profile…")]),
    ("ui.w13f.menu.reduce_colors", &[(Locale::En, "Reduce Colors…")]),
    ("ui.w13f.menu.wavelet", &[(Locale::En, "Wavelet Decompose…")]),
    ("ui.w13f.menu.clear_slices", &[(Locale::En, "Clear Slices")]),
    ("ui.w13f.menu.slices_from_guides", &[(Locale::En, "Slices from Guides")]),
    ("ui.w13f.menu.pattern_preview", &[(Locale::En, "Pattern Preview")]),
    // W13X-3: Layer > Layer Style > Scale Effects...
    ("ui.scale_effects.menu", &[(Locale::En, "Scale Effects…")]),
    ("ui.scale_effects.title", &[(Locale::En, "Scale Effects")]),
    ("ui.scale_effects.subtitle", &[(Locale::En, "Scales every size and distance of the active layer's style as one undo step.")]),
    ("ui.scale_effects.scale", &[(Locale::En, "Scale:")]),
    ("ui.scale_effects.percent_sign", &[(Locale::En, "%")]),
    ("ui.scale_effects.preview", &[(Locale::En, "Preview")]),
    ("ui.scale_effects.rendering", &[(Locale::En, "Rendering the preview…")]),
    ("ui.scale_effects.ok", &[(Locale::En, "OK")]),
    ("ui.scale_effects.unchanged", &[(Locale::En, "Scaling by 100% changes nothing")]),
    ("ui.w13f.why.profile_already", &[(Locale::En, "The document is already tagged with this profile")]),
    ("ui.w13f.why.wavelet_needs_srgb", &[(Locale::En, "Wavelet Decompose works on an sRGB document")]),
    ("ui.w13f.why.assign_needs_rgb", &[(Locale::En, "Profiles are assigned to RGB documents")]),
    ("ui.w13f.why.convert_needs_rgb", &[(Locale::En, "Profiles are converted between in RGB documents")]),
    ("ui.w13f.why.convert_depth", &[(Locale::En, "Convert to Profile works on 8- and 16-bit documents")]),
    ("ui.w13f.why.needs_rgb", &[(Locale::En, "This works on an RGB document")]),
    ("ui.w13f.why.needs_8bit", &[(Locale::En, "This works on an 8-bit document")]),
    ("ui.w13f.why.no_slices", &[(Locale::En, "There are no slices to clear")]),
    ("ui.w13f.why.no_guides", &[(Locale::En, "There are no guides to slice along")]),
    ("ui.w13f.ok", &[(Locale::En, "OK")]),
    ("ui.w13f.convert.title", &[(Locale::En, "Convert to Profile")]),
    ("ui.w13f.convert.subtitle", &[(Locale::En, "Rewrites every pixel layer's numbers so the picture looks the same under the destination profile, as one undo step.")]),
    ("ui.w13f.convert.source", &[(Locale::En, "Source Space")]),
    ("ui.w13f.convert.destination", &[(Locale::En, "Destination Space")]),
    ("ui.w13f.convert.options", &[(Locale::En, "Conversion Options")]),
    ("ui.w13f.convert.intent", &[(Locale::En, "Intent")]),
    ("ui.w13f.convert.bpc", &[(Locale::En, "Use Black Point Compensation")]),
    ("ui.w13f.convert.same", &[(Locale::En, "The destination is the profile the document already has")]),
    ("ui.w13f.convert.intent_note", &[(Locale::En, "These are matrix-shaper profiles, which carry no perceptual or saturation tables: those two intents convert as Relative Colorimetric.")]),
    ("ui.w13f.intent.perceptual", &[(Locale::En, "Perceptual")]),
    ("ui.w13f.intent.saturation", &[(Locale::En, "Saturation")]),
    ("ui.w13f.intent.relative", &[(Locale::En, "Relative Colorimetric")]),
    ("ui.w13f.intent.absolute", &[(Locale::En, "Absolute Colorimetric")]),
    ("ui.w13f.reduce.title", &[(Locale::En, "Reduce Colors")]),
    ("ui.w13f.reduce.subtitle", &[(Locale::En, "Maps the active layer onto a palette as one undo step; the document stays RGB.")]),
    ("ui.w13f.reduce.palette", &[(Locale::En, "Palette")]),
    ("ui.w13f.reduce.colors", &[(Locale::En, "Colors")]),
    ("ui.w13f.reduce.dither", &[(Locale::En, "Dither")]),
    ("ui.w13f.reduce.bad_count", &[(Locale::En, "A palette holds 2 to 256 colours")]),
    ("ui.w13f.wavelet.title", &[(Locale::En, "Wavelet Decompose")]),
    ("ui.w13f.wavelet.subtitle", &[(Locale::En, "Splits the active layer into Linear Light detail layers over a residual, finest on top; together they recomposite to the layer.")]),
    ("ui.w13f.wavelet.scales", &[(Locale::En, "Detail scales")]),
    ("ui.w13f.wavelet.bad_count", &[(Locale::En, "Wavelet Decompose splits into 2 to 7 scales")]),
    ("ui.w13f.status.no_document", &[(Locale::En, "No document is open")]),
    ("ui.w13f.status.not_a_row", &[(Locale::En, "{label} is not answered by this route")]),
    ("ui.w13f.status.icc_filter", &[(Locale::En, "ICC profile")]),
    ("ui.w13f.status.pick_profile", &[(Locale::En, "Choose a colour profile")]),
    ("ui.w13f.status.no_profile_file", &[(Locale::En, "No profile file was chosen")]),
    ("ui.w13f.status.file_error", &[(Locale::En, "{path}: {error}")]),
    ("ui.w13f.status.profile_too_big", &[(Locale::En, "{path} is {len} bytes, larger than any colour profile")]),
    ("ui.w13f.status.profile_not_rgb", &[(Locale::En, "{path} is a {space} profile; an RGB document takes an RGB profile")]),
    ("ui.w13f.status.untransformable", &[(Locale::En, "the profile cannot be transformed ({error})")]),
    ("ui.w13f.status.needs_rgb_document", &[(Locale::En, "Profiles belong to RGB documents; convert to RGB first")]),
    ("ui.w13f.status.already_tagged", &[(Locale::En, "The document is already tagged {name}")]),
    ("ui.w13f.status.assigned", &[(Locale::En, "Assigned {name}: the pixel numbers are unchanged, so the colours look different")]),
    ("ui.w13f.status.own_profile", &[(Locale::En, "The document's own profile: {error}")]),
    ("ui.w13f.status.already_in", &[(Locale::En, "The document is already in {name}")]),
    ("ui.w13f.status.refused", &[(Locale::En, "{label} was refused: {reason}")]),
    ("ui.w13f.status.layer_not_rewritten", &[(Locale::En, "a layer could not be rewritten")]),
    ("ui.w13f.status.converted", &[(Locale::En, "Converted to {name} ({intent}): the numbers changed so the colours look the same")]),
    ("ui.w13f.status.select_layer", &[(Locale::En, "Select a layer first")]),
    ("ui.w13f.status.layer_missing", &[(Locale::En, "The active layer is not in the document")]),
    ("ui.w13f.status.not_pixel_layer", &[(Locale::En, "This works on a pixel layer; the active layer is a {kind}")]),
    ("ui.w13f.status.locked", &[(Locale::En, "The layer's pixels are locked")]),
    ("ui.w13f.status.reduce_nothing", &[(Locale::En, "Reduce Colors changed nothing: the layer already uses only colours of that palette")]),
    ("ui.w13f.status.reduced", &[(Locale::En, "Reduce Colors: the layer now uses at most {n} colours")]),
    ("ui.w13f.status.wavelet_other_space", &[(Locale::En, "{reason}; this one is {space}")]),
    ("ui.w13f.status.wavelet_empty", &[(Locale::En, "Wavelet Decompose: the layer has no pixels")]),
    ("ui.w13f.status.layers_not_added", &[(Locale::En, "the layers could not be added")]),
    ("ui.w13f.status.decomposed", &[(Locale::En, "Wavelet Decompose: {n} detail layers over a residual; the source layer is hidden")]),
    ("ui.w13f.status.residual", &[(Locale::En, "{name} Residual")]),
    ("ui.w13f.status.scale", &[(Locale::En, "{name} Scale {k}")]),
    ("ui.w13f.status.cleared_slices", &[(Locale::En, "Cleared {n} slice(s)")]),
    ("ui.w13f.status.no_guide_crosses", &[(Locale::En, "No guide crosses the canvas")]),
    ("ui.w13f.status.sliced", &[(Locale::En, "{n} slices from the guides ({cols} x {rows}); they replace the slices there were")]),
    // W10-B: the Layer Comps, Tool Presets, Glyphs, Notes and Character /
    // Paragraph Styles panels.
    ("ui.glyphs.no_document", &[(Locale::En, "Open a document to insert glyphs into its text.")]),
    ("ui.glyphs.no_text", &[(Locale::En, "Select a text layer, or type into one: a click inserts the glyph at the caret.")]),
    ("ui.glyphs.font", &[(Locale::En, "Font: ")]),
    ("ui.glyphs.default_font", &[(Locale::En, "Default")]),
    ("ui.layer_comps.no_document", &[(Locale::En, "Open a document to record layer comps.")]),
    ("ui.layer_comps.none", &[(Locale::En, "No layer comps yet. New Layer Comp records every layer's visibility, position and appearance.")]),
    ("ui.layer_comps.new", &[(Locale::En, "New layer comp")]),
    ("ui.layer_comps.update", &[(Locale::En, "Update the applied comp from the layers")]),
    ("ui.layer_comps.delete", &[(Locale::En, "Delete the applied comp")]),
    ("ui.layer_comps.previous", &[(Locale::En, "Apply the previous comp")]),
    ("ui.layer_comps.next", &[(Locale::En, "Apply the next comp")]),
    ("ui.layer_comps.applied", &[(Locale::En, "Applied")]),
    ("ui.notes.no_document", &[(Locale::En, "Open a document to pin notes on it.")]),
    ("ui.notes.none", &[(Locale::En, "No notes yet. Click the canvas with the Note tool (I), or New Note pins one at the centre of the view; notes are saved with the document and never exported.")]),
    ("ui.notes.new", &[(Locale::En, "New note at the centre of the view")]),
    ("ui.notes.show", &[(Locale::En, "Centre the view on this note")]),
    ("ui.notes.delete", &[(Locale::En, "Delete this note")]),
    ("ui.css.no_document", &[(Locale::En, "Open a document to see its CSS")]),
    ("ui.css.no_layer", &[(Locale::En, "Select a layer to see its CSS")]),
    ("ui.css.copy", &[(Locale::En, "Copy")]),
    // W13-N: the Styles, Document Info and Guide Guy panels, Magic Cut, and
    // File > Automate > Resize Images / Generate Mockups.
    ("ui.styles.no_document", &[(Locale::En, "Open a document to apply styles to its layers.")]),
    ("ui.styles.none", &[(Locale::En, "No styles yet. The + button saves the active layer's style; File > Open loads an .asl style library.")]),
    ("ui.styles.new", &[(Locale::En, "New style from the active layer")]),
    ("ui.styles.no_layer", &[(Locale::En, "Select a layer: a click on a style applies it to the active layer.")]),
    ("ui.doc_info.no_document", &[(Locale::En, "Open a document to see its facts.")]),
    ("ui.doc_info.size", &[(Locale::En, "Size")]),
    ("ui.doc_info.print_size", &[(Locale::En, "Print size")]),
    ("ui.doc_info.resolution", &[(Locale::En, "Resolution")]),
    ("ui.doc_info.mode", &[(Locale::En, "Mode")]),
    ("ui.doc_info.profile", &[(Locale::En, "Profile")]),
    ("ui.doc_info.layers", &[(Locale::En, "Layers")]),
    ("ui.doc_info.memory", &[(Locale::En, "Memory")]),
    ("ui.guide_guy.no_document", &[(Locale::En, "Open a document to lay guides out on it.")]),
    ("ui.guide_guy.margins", &[(Locale::En, "Margins")]),
    ("ui.guide_guy.top", &[(Locale::En, "Top")]),
    ("ui.guide_guy.left", &[(Locale::En, "Left")]),
    ("ui.guide_guy.bottom", &[(Locale::En, "Bottom")]),
    ("ui.guide_guy.right", &[(Locale::En, "Right")]),
    ("ui.guide_guy.columns", &[(Locale::En, "Columns")]),
    ("ui.guide_guy.rows", &[(Locale::En, "Rows")]),
    ("ui.guide_guy.gutter", &[(Locale::En, "Gutter")]),
    ("ui.guide_guy.center_vertical", &[(Locale::En, "Vertical center guide")]),
    ("ui.guide_guy.center_horizontal", &[(Locale::En, "Horizontal center guide")]),
    ("ui.guide_guy.replace", &[(Locale::En, "Replace existing guides")]),
    ("ui.guide_guy.apply", &[(Locale::En, "Apply Guides")]),
    ("ui.guide_guy.no_room", &[(Locale::En, "The margins and gutters leave no room for a column or a row.")]),
    ("ui.magic_cut.title", &[(Locale::En, "Magic Cut")]),
    ("ui.magic_cut.subtitle", &[(Locale::En, "Paint over what to keep and over what to drop")]),
    ("ui.magic_cut.hint", &[(Locale::En, "Foreground strokes mark what to keep, background strokes what to drop; Preview shows the cut.")]),
    ("ui.magic_cut.brush.foreground", &[(Locale::En, "Brush: Foreground")]),
    ("ui.magic_cut.brush.background", &[(Locale::En, "Brush: Background")]),
    ("ui.magic_cut.brush.size", &[(Locale::En, "Size")]),
    ("ui.magic_cut.smooth", &[(Locale::En, "Smooth")]),
    ("ui.magic_cut.shift", &[(Locale::En, "Shift Edge")]),
    ("ui.magic_cut.feather", &[(Locale::En, "Feather")]),
    ("ui.magic_cut.output", &[(Locale::En, "Output")]),
    ("ui.magic_cut.output.selection", &[(Locale::En, "Selection")]),
    ("ui.magic_cut.output.mask", &[(Locale::En, "Layer Mask")]),
    ("ui.magic_cut.output.layer", &[(Locale::En, "New Layer")]),
    ("ui.magic_cut.ok", &[(Locale::En, "OK")]),
    ("ui.magic_cut.preview", &[(Locale::En, "Preview")]),
    ("ui.magic_cut.clear", &[(Locale::En, "Clear Strokes")]),
    ("ui.magic_cut.need_foreground", &[(Locale::En, "Paint over the part to keep with the foreground brush first.")]),
    ("ui.magic_cut.need_background", &[(Locale::En, "Nothing is left to learn the background from: paint over it with the background brush.")]),
    ("ui.magic_cut.no_pixels", &[(Locale::En, "The layer has no pixels to cut.")]),
    ("ui.magic_cut.empty", &[(Locale::En, "The cut is empty: paint more of the part to keep.")]),
    ("ui.merge_channels.title", &[(Locale::En, "Merge Channels")]),
    ("ui.merge_channels.subtitle", &[(Locale::En, "Choose the open grayscale document each channel of the new RGB document comes from.")]),
    ("ui.merge_channels.red", &[(Locale::En, "Red")]),
    ("ui.merge_channels.green", &[(Locale::En, "Green")]),
    ("ui.merge_channels.blue", &[(Locale::En, "Blue")]),
    ("ui.merge_channels.ok", &[(Locale::En, "Merge")]),
    // W13X-4: the Channels panel menu and New Spot Channel.
    ("ui.spot_channel.title", &[(Locale::En, "New Spot Channel")]),
    ("ui.spot_channel.subtitle", &[(Locale::En, "The ink is laid over the image; the selection, if there is one, is where it prints.")]),
    ("ui.spot_channel.name", &[(Locale::En, "Name")]),
    ("ui.spot_channel.ink", &[(Locale::En, "Ink Color")]),
    ("ui.spot_channel.solidity", &[(Locale::En, "Solidity")]),
    ("ui.spot_channel.ok", &[(Locale::En, "OK")]),
    ("ui.spot_channel.no_name", &[(Locale::En, "Name the channel first.")]),
    ("ui.docks.channels.menu.new.spot", &[(Locale::En, "New Spot Channel…")]),
    ("ui.docks.channels.menu.merge", &[(Locale::En, "Merge Channels…")]),
    ("ui.docks.channels.menu.no.document", &[(Locale::En, "Open a document first")]),
    // W16-E: the preset, History and Channels panel menus, the mask popups,
    // the Layer Comps flags, the Notes author and the Navigator angle.
    ("ui.w16.menu.open.aco", &[(Locale::En, "Open .ACO…")]),
    ("ui.w16.menu.open.abr", &[(Locale::En, "Open .ABR…")]),
    ("ui.w16.menu.open.asl", &[(Locale::En, "Open .ASL…")]),
    ("ui.w16.menu.export.aco", &[(Locale::En, "Export as .ACO…")]),
    ("ui.w16.menu.export.abr", &[(Locale::En, "Export as .ABR…")]),
    ("ui.w16.menu.export.asl", &[(Locale::En, "Export as .ASL…")]),
    ("ui.w16.menu.nothing.selected", &[(Locale::En, "Click an item in the panel first")]),
    ("ui.w16.menu.library.empty", &[(Locale::En, "The list is empty")]),
    ("ui.w16.menu.rename", &[(Locale::En, "Name Change")]),
    ("ui.w16.menu.delete", &[(Locale::En, "Delete")]),
    ("ui.w16.menu.tiles.list", &[(Locale::En, "Tiles/List")]),
    ("ui.w16.menu.define.new", &[(Locale::En, "Define New")]),
    ("ui.w16.menu.new.folder", &[(Locale::En, "New Folder")]),
    ("ui.w16.swatches.new.folder", &[(Locale::En, "New folder")]),
    ("ui.w16.swatches.folder.toggle", &[(Locale::En, "Open or close the folder")]),
    ("ui.w16.history.no.document", &[(Locale::En, "Open a document first")]),
    ("ui.w16.history.nothing.to.clear", &[(Locale::En, "There is no history to clear")]),
    ("ui.w16.history.clear", &[(Locale::En, "Clear History")]),
    ("ui.w16.history.new.snapshot", &[(Locale::En, "New Snapshot")]),
    ("ui.w16.channels.gone", &[(Locale::En, "That channel is no longer in the document")]),
    ("ui.w16.channels.color.not.deletable", &[(Locale::En, "A colour channel cannot be deleted")]),
    ("ui.w16.channels.menu.new", &[(Locale::En, "New")]),
    ("ui.w16.channels.menu.delete", &[(Locale::En, "Delete")]),
    ("ui.w16.channels.spot.delete", &[(Locale::En, "Delete this spot channel")]),
    ("ui.w16.channels.spot.options", &[(Locale::En, "Spot Channel Options…")]),
    ("ui.w16.mask.delete", &[(Locale::En, "Delete")]),
    ("ui.w16.mask.apply", &[(Locale::En, "Apply")]),
    ("ui.w16.vector.mask.disable", &[(Locale::En, "Disable Vector Mask")]),
    ("ui.w16.vector.mask.enable", &[(Locale::En, "Enable Vector Mask")]),
    ("ui.w16.vector.mask.delete", &[(Locale::En, "Delete Vector Mask")]),
    ("ui.w16.navigator.angle", &[(Locale::En, "Angle")]),
    // W16-K: the Zoom, Rotate View and Crop options-bar captions, Photopea's words.
    ("ui.w16k.bar.pixel_to_pixel", &[(Locale::En, "Pixel to Pixel")]),
    ("ui.w16k.bar.fit_the_area", &[(Locale::En, "Fit The Area")]),
    ("ui.w16k.bar.reset", &[(Locale::En, "Reset")]),
    ("ui.w16k.crop_by.all_layers", &[(Locale::En, "All Layers")]),
    ("ui.w16k.crop_by.current_layer", &[(Locale::En, "Current Layer")]),
    ("ui.w16k.crop_by.trim", &[(Locale::En, "Trim")]),
    ("ui.w16k.crop_by.selection", &[(Locale::En, "Selection")]),
    ("ui.w16.navigator.degrees", &[(Locale::En, "\u{b0}")]),
    ("ui.w16.comps.last.state", &[(Locale::En, "Last Document State")]),
    ("ui.w16.comps.last.state.none", &[(Locale::En, "Kept when a comp is first applied")]),
    ("ui.w16.comps.flag.visibility", &[(Locale::En, "Visibility")]),
    ("ui.w16.comps.flag.position", &[(Locale::En, "Position")]),
    ("ui.w16.comps.flag.appearance", &[(Locale::En, "Appearance")]),
    ("ui.w16.notes.author", &[(Locale::En, "Author")]),
    ("ui.docks.channels.spot.hint", &[(Locale::En, "Spot channel: composited over the image as its ink")]),
    ("ui.folder_job.resize.title", &[(Locale::En, "Resize Images")]),
    ("ui.folder_job.mockups.title", &[(Locale::En, "Generate Mockups")]),
    ("ui.folder_job.resize.subtitle", &[(Locale::En, "Every image in the source folder is fitted into the box and written to the destination folder in its own format.")]),
    ("ui.folder_job.mockups.subtitle", &[(Locale::En, "The active smart object shows each image of the source folder in turn; each result is exported as a PNG to the destination folder.")]),
    ("ui.folder_job.need_folders", &[(Locale::En, "Choose the source and the destination folders.")]),
    ("ui.folder_job.same_folder", &[(Locale::En, "The destination must be another folder than the source.")]),
    ("ui.folder_job.width", &[(Locale::En, "Width")]),
    ("ui.folder_job.height", &[(Locale::En, "Height")]),
    ("ui.folder_job.enlarge", &[(Locale::En, "Enlarge smaller images")]),
    ("ui.folder_job.run", &[(Locale::En, "Run")]),
    ("ui.tool_presets.none", &[(Locale::En, "No tool presets yet. New Tool Preset saves the active tool with its current options.")]),
    ("ui.tool_presets.new", &[(Locale::En, "New tool preset from the active tool")]),
    ("ui.tool_presets.delete", &[(Locale::En, "Delete the selected tool preset")]),
    ("ui.text_styles.no_document", &[(Locale::En, "Open a document to keep text styles in it.")]),
    ("ui.text_styles.in_use", &[(Locale::En, "In use")]),
    ("ui.text_styles.noun.character", &[(Locale::En, "Character Style")]),
    ("ui.text_styles.noun.paragraph", &[(Locale::En, "Paragraph Style")]),
    ("ui.text_styles.empty.character", &[(Locale::En, "No character styles yet. New records the active text layer's font, size and look.")]),
    ("ui.text_styles.empty.paragraph", &[(Locale::En, "No paragraph styles yet. New records the active text layer's alignment, indents and spacing.")]),
    ("ui.text_styles.delete", &[(Locale::En, "Delete the selected {noun}")]),
    ("ui.text_styles.new", &[(Locale::En, "New {noun} from the active text layer")]),
    ("ui.text_styles.redefine", &[(Locale::En, "Redefine the selected {noun} from the active text layer, restyling every layer that uses it")]),
    ("ui.text_styles.clear", &[(Locale::En, "Clear the {noun} from the active text layer")]),
    // W3-H: Color Range, Select ▸ Modify, Save / Load Selection.
    ("ui.selection_modify.border.title", &[(Locale::En, "Border Selection")]),
    ("ui.selection_modify.smooth.title", &[(Locale::En, "Smooth Selection")]),
    ("ui.selection_modify.expand.title", &[(Locale::En, "Expand Selection")]),
    ("ui.selection_modify.contract.title", &[(Locale::En, "Contract Selection")]),
    ("ui.selection_modify.feather.title", &[(Locale::En, "Feather Selection")]),
    ("ui.selection_modify.width", &[(Locale::En, "Width")]),
    ("ui.selection_modify.sample.radius", &[(Locale::En, "Sample radius")]),
    ("ui.selection_modify.expand.by", &[(Locale::En, "Expand by")]),
    ("ui.selection_modify.contract.by", &[(Locale::En, "Contract by")]),
    ("ui.selection_modify.feather.radius", &[(Locale::En, "Feather radius")]),
    ("ui.selection_modify.apply", &[(Locale::En, "Apply")]),
    ("ui.selection_modify.px", &[(Locale::En, "px")]),
    ("ui.selection_modify.out.of.range", &[(Locale::En, "The amount must be within")]),
    ("ui.selection_modify.range.to", &[(Locale::En, "to")]),
    ("ui.selection_modify.border.caption", &[(Locale::En, "Selects a band of this width along the selection's edge")]),
    ("ui.selection_name.alpha", &[(Locale::En, "Alpha")]),
    ("ui.selection_name.save.title", &[(Locale::En, "Save Selection")]),
    ("ui.selection_name.load.title", &[(Locale::En, "Load Selection")]),
    ("ui.selection_name.empty", &[(Locale::En, "The name cannot be empty")]),
    ("ui.selection_name.taken", &[(Locale::En, "A saved selection already has that name")]),
    ("ui.selection_name.name", &[(Locale::En, "Name")]),
    ("ui.selection_name.save.caption", &[(Locale::En, "The selection is kept with the document under this name")]),
    ("ui.selection_name.save", &[(Locale::En, "Save")]),
    ("ui.selection_name.op.new", &[(Locale::En, "New Selection")]),
    ("ui.selection_name.op.add", &[(Locale::En, "Add to Selection")]),
    ("ui.selection_name.op.subtract", &[(Locale::En, "Subtract from Selection")]),
    ("ui.selection_name.op.intersect", &[(Locale::En, "Intersect with Selection")]),
    ("ui.selection_name.op.needs.selection", &[(Locale::En, "There is no live selection to combine with")]),
    ("ui.selection_name.none.saved", &[(Locale::En, "No selection has been saved")]),
    ("ui.selection_name.channel", &[(Locale::En, "Channel")]),
    ("ui.selection_name.operation", &[(Locale::En, "Operation")]),
    ("ui.selection_name.invert", &[(Locale::En, "Invert")]),
    ("ui.selection_name.load", &[(Locale::En, "Load")]),
    ("ui.color_range.title", &[(Locale::En, "Color Range")]),
    ("ui.color_range.subtitle", &[(Locale::En, "Select every pixel near one colour")]),
    ("ui.color_range.view.selection", &[(Locale::En, "Selection")]),
    ("ui.color_range.view.image", &[(Locale::En, "Image")]),
    ("ui.color_range.sampled.colour", &[(Locale::En, "Sampled colour")]),
    ("ui.color_range.fuzziness", &[(Locale::En, "Fuzziness")]),
    ("ui.color_range.invert", &[(Locale::En, "Invert")]),
    ("ui.color_range.click.to.sample", &[(Locale::En, "Click anywhere to sample a colour")]),
    ("ui.color_range.click.preview", &[(Locale::En, "Click the preview to sample a colour from it")]),
    ("ui.color_range.select", &[(Locale::En, "Select")]),
    ("ui.color_range.eyedropper", &[(Locale::En, "Eyedropper")]),
    ("actions.record", &[(Locale::En, "Record")]),
    ("ui.canvas_rotation.rotates.everything", &[(Locale::En, "Rotates the canvas and every layer. Right angles are pixel-exact; other angles resample.")]),
    ("ui.docks.enter.a.colour", &[(Locale::En, "Enter a colour like #3366CC")]),
    ("ui.docks.no.layers.yet", &[(Locale::En, "No layers yet. Add one with the + button below.")]),
    ("ui.docks.show.hide.layer", &[(Locale::En, "Show / hide layer")]),
    ("ui.docks.show.hide.channel", &[(Locale::En, "Show / hide this channel")]),
    ("ui.docks.show.hide.path", &[(Locale::En, "Show / hide this path")]),
    ("ui.toolbar.background.picker", &[(Locale::En, "Background — double-click for the picker")]),
    ("ui.toolbar.gradient.stops", &[(Locale::En, "Edit gradient stops — click to open the editor")]),
    ("ui.toolbar.foreground.picker", &[(Locale::En, "Foreground — double-click for the picker")]),
    ("ui.canvas_size.smaller.clips", &[(Locale::En, "The new canvas is smaller — content outside it will be clipped.")]),
    ("ui.color_picker.before.after", &[(Locale::En, "before / after")]),
    ("ui.export_as.16.bit", &[(Locale::En, "16 bit")]),
    ("ui.export_as.lab.as.rgb", &[(Locale::En, "This is a Lab document. No format here stores Lab, so it is converted to RGB on export.")]),
    ("ui.export_as.cmyk.written", &[(Locale::En, "Written as CMYK: a naive ink model, not an ICC press profile.")]),
    ("ui.export_as.cmyk.as.rgb", &[(Locale::En, "This format cannot store CMYK, so the document is written as RGB. Choose JPEG or TIFF for CMYK.")]),
    ("ui.export_as.indexed.written", &[(Locale::En, "Written with the document's palette.")]),
    ("ui.export_as.indexed.as.rgb", &[(Locale::En, "This format has no palette, so the indexed colours are written as RGB. Choose PNG or GIF for the palette.")]),
    ("ui.export_as.8.bit", &[(Locale::En, "8 bit")]),
    ("ui.export_as.exif.not.implemented", &[(Locale::En, "EXIF and XMP writing is not implemented — only ICC is embedded")]),
    ("ui.fill_stroke.50.grey", &[(Locale::En, "50% Grey")]),
    ("ui.fill_stroke.content.aware", &[(Locale::En, "Content-Aware")]),
    ("ui.fill_stroke.opacity.range", &[(Locale::En, "Opacity must be between 0% and 100%")]),
    ("ui.filter_gallery.pick.a.filter", &[(Locale::En, "Pick a filter; it applies at its default settings.")]),
    ("ui.image_size.hard.edges", &[(Locale::En, "Hard edges, no blending. Pixel art only — it aliases on downscale.")]),
    ("ui.layer_style.bevel.emboss", &[(Locale::En, "Bevel & Emboss")]),
    ("ui.layer_style.no.pattern", &[(Locale::En, "No pattern chosen — the overlay paints nothing.")]),
    ("ui.units.0.bytes", &[(Locale::En, "0 bytes")]),
    ("ui.preferences.minutes.0.is.off", &[(Locale::En, "minutes (0 is off)")]),
    ("ui.preferences.ui.scale", &[(Locale::En, "UI scale")]),
    ("ui.preferences.language", &[(Locale::En, "Language")]),
    ("ui.preferences.only.english", &[(Locale::En, "Only English is available in this build")]),
    ("ui.preferences.units", &[(Locale::En, "Units")]),
    ("ui.preferences.units.caption", &[(Locale::En, "The rulers and the size readouts use this unit")]),
    ("ui.preferences.scroll.wheel.zooms.instead.of.scrolling", &[(Locale::En, "Scroll wheel zooms instead of scrolling")]),
    ("ui.preferences.scroll.wheel.caption", &[(Locale::En, "Off: the wheel pans the view and Ctrl+wheel zooms")]),
    ("ui.preferences.scratch.directory", &[(Locale::En, "Scratch directory")]),
    ("ui.preferences.scratch.caption", &[(Locale::En, "Autosaves of never-saved documents go here; empty uses the default")]),
    ("ui.preferences.press.a.key", &[(Locale::En, "Press a key\u{2026}")]),
    ("ui.preferences.add.shortcut", &[(Locale::En, "Add shortcut")]),
    ("ui.preferences.remove.this.shortcut", &[(Locale::En, "Remove this shortcut")]),
    ("ui.preferences.changed", &[(Locale::En, "changed")]),
    ("ui.preferences.no.commands", &[(Locale::En, "No commands to bind")]),
    ("ui.preferences.reassign.anyway", &[(Locale::En, "Reassign anyway")]),
    ("ui.preferences.keep.as.it.was", &[(Locale::En, "Keep as it was")]),
    ("ui.preferences.reset.all.shortcuts", &[(Locale::En, "Reset all shortcuts")]),
    ("ui.keymap.no.such.command", &[(Locale::En, "No such command")]),
    ("ui.keymap.already.used.by", &[(Locale::En, "Already used by")]),
    ("ui.docks.expand.the.dock", &[(Locale::En, "Expand the dock")]),
    ("ui.docks.character.face.none", &[(Locale::En, "No faces listed for this family name")]),
    ("ui.docks.character.family.not.installed", &[(Locale::En, "Not installed — shaping with")]),
    ("ui.docks.character.no.matching.family", &[(Locale::En, "No installed family matches")]),
    ("ui.docks.paragraph.boxed", &[(Locale::En, "Wrap to box")]),
    ("ui.docks.paragraph.box.height", &[(Locale::En, "Box height")]),
    ("ui.docks.paragraph.fixed.height", &[(Locale::En, "Fixed height")]),
    ("ui.docks.paragraph.overset", &[(Locale::En, "Text overflows the box")]),
    ("ui.docks.paragraph.overset.lines", &[(Locale::En, "lines past the box")]),
    ("ui.docks.close.panel", &[(Locale::En, "Close panel")]),
    ("ui.docks.move.this.panel", &[(Locale::En, "Move this panel")]),
    ("ui.docks.move.to", &[(Locale::En, "Move to")]),
    ("ui.docks.the.panel.is.already.on.this", &[(Locale::En, "The panel is already on this side")]),
    ("ui.docks.dock.this.panel.here", &[(Locale::En, "Dock this panel here")]),
    ("ui.docks.move.this.panel.within.its.dock", &[(Locale::En, "Move this panel within its dock")]),
    ("ui.docks.this.panel.is.already.at.the", &[(Locale::En, "This panel is already at the top of its dock")]),
    ("ui.docks.this.panel.is.already.at.the.2", &[(Locale::En, "This panel is already at the bottom of its dock")]),
    ("ui.docks.no.drop", &[(Locale::En, "no drop")]),
    ("ui.docks.mask.off", &[(Locale::En, "mask off")]),
    // Card 055: the two thumbnail wells of a layer row.
    (
        "ui.docks.content.thumbnail",
        &[(Locale::En, "content thumbnail — click to edit the layer's pixels")],
    ),
    (
        "ui.docks.mask.thumbnail",
        &[(Locale::En, "mask thumbnail — click to edit the layer's mask")],
    ),
    // W9-G: the vector mask's thumbnail well and its Properties rows.
    (
        "ui.docks.vector.mask.thumbnail",
        &[(
            Locale::En,
            "vector mask thumbnail — click to edit the vector mask",
        )],
    ),
    ("ui.docks.vector.mask", &[(Locale::En, "Vector mask")]),
    ("ui.docks.pixel.mask", &[(Locale::En, "Pixel mask")]),
    ("ui.docks.vector.density", &[(Locale::En, "Vector density")]),
    ("ui.docks.vector.feather", &[(Locale::En, "Vector feather")]),
    ("ui.docks.vector.invert", &[(Locale::En, "Vector invert")]),
    ("ui.docks.vector.enabled", &[(Locale::En, "Vector enabled")]),
    (
        "ui.docks.mask.target.badge",
        &[(
            Locale::En,
            "edits are aimed at this mask — right-click for view and mask options",
        )],
    ),
    ("ui.docks.mask.disable", &[(Locale::En, "Disable Mask")]),
    ("ui.docks.mask.enable", &[(Locale::En, "Enable Mask")]),
    ("ui.docks.mask.toggle.link", &[(Locale::En, "Toggle Link")]),
    ("ui.docks.mask.view.composite", &[(Locale::En, "Composite")]),
    ("ui.docks.mask.view.grayscale", &[(Locale::En, "Grayscale")]),
    ("ui.docks.mask.view.overlay", &[(Locale::En, "Overlay")]),
    (
        "ui.refine_mask.subtitle",
        &[(
            Locale::En,
            "Edge refinement: feather, shift, smooth, and contrast over the mask's existing coverage — not subject recognition.",
        )],
    ),
    ("ui.refine_mask.feather", &[(Locale::En, "Feather")]),
    ("ui.refine_mask.shift", &[(Locale::En, "Shift Edge")]),
    ("ui.refine_mask.smooth", &[(Locale::En, "Smooth")]),
    ("ui.refine_mask.contrast", &[(Locale::En, "Contrast")]),
    ("ui.refine_mask.background.label", &[(Locale::En, "Preview against")]),
    ("ui.refine_mask.background.black", &[(Locale::En, "Black")]),
    ("ui.refine_mask.background.white", &[(Locale::En, "White")]),
    ("ui.refine_mask.background.checker", &[(Locale::En, "Checkerboard")]),
    ("ui.refine_mask.confirm", &[(Locale::En, "Refine")]),
    ("ui.refine_mask.title", &[(Locale::En, "Refine Mask")]),
    (
        "ui.defringe.subtitle",
        &[(
            Locale::En,
            "Color fringe cleanup: pulls the boundary pixels' color toward the nearby interior ink — a separate edit from mask refinement.",
        )],
    ),
    ("ui.defringe.radius", &[(Locale::En, "Radius")]),
    ("ui.defringe.strength", &[(Locale::En, "Strength")]),
    ("ui.defringe.confirm", &[(Locale::En, "Remove Fringe")]),
    ("ui.defringe.title", &[(Locale::En, "Remove Color Fringe")]),
    (
        "ui.defringe.nothing.to.clean",
        &[(Locale::En, "Every parameter is at its neutral value — there is nothing to clean")],
    ),
    ("ui.refine_mask.px.suffix", &[(Locale::En, " px")]),
    (
        "ui.refine_mask.nothing.to.refine",
        &[(Locale::En, "Every parameter is at its neutral value — there is nothing to refine")],
    ),
    // W2-F: the About, Trim, New Guide and Rename Layer dialogs.
    ("ui.about.title", &[(Locale::En, "About Raster Studio")]),
    ("ui.about.tagline", &[(Locale::En, "A layered raster editor")]),
    ("ui.about.third.party.notices", &[(Locale::En, "Third-party notices")]),
    ("ui.trim.subtitle", &[(Locale::En, "Crops the canvas to the content, judged by the basis below.")]),
    ("ui.trim.based.on", &[(Locale::En, "Based On")]),
    ("ui.trim.transparent.pixels", &[(Locale::En, "Transparent Pixels")]),
    ("ui.trim.top.left.color", &[(Locale::En, "Top Left Pixel Color")]),
    ("ui.trim.bottom.right.color", &[(Locale::En, "Bottom Right Pixel Color")]),
    ("ui.trim.trim.away", &[(Locale::En, "Trim Away")]),
    ("ui.trim.choose.a.side", &[(Locale::En, "Choose at least one side to trim")]),
    // W7-D: Image > Mode > Indexed Color.
    ("ui.indexed.title", &[(Locale::En, "Indexed Color")]),
    ("ui.indexed.subtitle", &[(Locale::En, "Maps every colour onto one palette. The document keeps RGBA pixels; GIF and PNG export write this palette.")]),
    ("ui.indexed.palette", &[(Locale::En, "Palette")]),
    ("ui.indexed.colors", &[(Locale::En, "Colors")]),
    ("ui.indexed.dither", &[(Locale::En, "Dither")]),
    ("ui.indexed.exact", &[(Locale::En, "Exact")]),
    ("ui.indexed.web", &[(Locale::En, "Web (216)")]),
    ("ui.indexed.uniform", &[(Locale::En, "Uniform")]),
    ("ui.indexed.adaptive", &[(Locale::En, "Adaptive (median cut)")]),
    ("ui.indexed.dither.none", &[(Locale::En, "None")]),
    ("ui.indexed.dither.diffusion", &[(Locale::En, "Diffusion")]),
    ("ui.indexed.bad.count", &[(Locale::En, "Choose 2 to 256 colours")]),
    // W10-H: Image > Mode > Bitmap / Duotone, Image > Apply Image / Calculations.
    ("ui.bitmap.title", &[(Locale::En, "Bitmap")]),
    ("ui.bitmap.subtitle", &[(Locale::En, "Reduces the grayscale image to pure black and white. The layers are flattened into one, over white.")]),
    ("ui.bitmap.method", &[(Locale::En, "Method")]),
    ("ui.bitmap.method.threshold", &[(Locale::En, "50% Threshold")]),
    ("ui.bitmap.method.pattern", &[(Locale::En, "Pattern Dither")]),
    ("ui.bitmap.method.diffusion", &[(Locale::En, "Diffusion Dither")]),
    ("ui.bitmap.method.halftone", &[(Locale::En, "Halftone Screen")]),
    ("ui.bitmap.cell", &[(Locale::En, "Cell size (pixels)")]),
    ("ui.bitmap.angle", &[(Locale::En, "Angle (degrees)")]),
    ("ui.bitmap.shape", &[(Locale::En, "Shape")]),
    ("ui.bitmap.shape.round", &[(Locale::En, "Round")]),
    ("ui.bitmap.shape.square", &[(Locale::En, "Square")]),
    ("ui.bitmap.shape.diamond", &[(Locale::En, "Diamond")]),
    ("ui.bitmap.shape.line", &[(Locale::En, "Line")]),
    ("ui.bitmap.bad.cell", &[(Locale::En, "The cell size must be 2 to 200 pixels")]),
    ("ui.duotone.title", &[(Locale::En, "Duotone Options")]),
    ("ui.duotone.subtitle", &[(Locale::En, "Prints the grayscale image through one to four inks, each through its own curve. The inks are applied to the pixels when the mode changes.")]),
    ("ui.duotone.type", &[(Locale::En, "Type")]),
    ("ui.duotone.type.mono", &[(Locale::En, "Monotone")]),
    ("ui.duotone.type.duo", &[(Locale::En, "Duotone")]),
    ("ui.duotone.type.tri", &[(Locale::En, "Tritone")]),
    ("ui.duotone.type.quad", &[(Locale::En, "Quadtone")]),
    ("ui.duotone.ink", &[(Locale::En, "Ink")]),
    ("ui.duotone.curve", &[(Locale::En, "Ink printed at 0, 25, 50, 75 and 100% tint")]),
    ("ui.duotone.preview", &[(Locale::En, "Black to white through the inks")]),
    ("ui.apply_image.title", &[(Locale::En, "Apply Image")]),
    ("ui.apply_image.subtitle", &[(Locale::En, "Blends a source document, layer or channel into the active layer as one step.")]),
    ("ui.apply_image.source", &[(Locale::En, "Source")]),
    ("ui.apply_image.document", &[(Locale::En, "Document")]),
    ("ui.apply_image.layer", &[(Locale::En, "Layer")]),
    ("ui.apply_image.merged", &[(Locale::En, "Merged")]),
    ("ui.apply_image.channel", &[(Locale::En, "Channel")]),
    ("ui.apply_image.invert", &[(Locale::En, "Invert")]),
    ("ui.apply_image.blending", &[(Locale::En, "Blending")]),
    ("ui.apply_image.opacity", &[(Locale::En, "Opacity")]),
    ("ui.apply_image.preserve", &[(Locale::En, "Preserve Transparency")]),
    ("ui.apply_image.use.mask", &[(Locale::En, "Mask")]),
    ("ui.apply_image.mask", &[(Locale::En, "Mask source")]),
    ("ui.apply_image.channel.rgb", &[(Locale::En, "RGB")]),
    ("ui.apply_image.channel.red", &[(Locale::En, "Red")]),
    ("ui.apply_image.channel.green", &[(Locale::En, "Green")]),
    ("ui.apply_image.channel.blue", &[(Locale::En, "Blue")]),
    ("ui.apply_image.channel.gray", &[(Locale::En, "Gray")]),
    ("ui.apply_image.channel.transparency", &[(Locale::En, "Transparency")]),
    ("ui.apply_image.no.source", &[(Locale::En, "No open document is the same size as this one")]),
    ("ui.calculations.title", &[(Locale::En, "Calculations")]),
    ("ui.calculations.subtitle", &[(Locale::En, "Blends one channel of each of two sources into a new channel, a selection or a new document.")]),
    ("ui.calculations.source1", &[(Locale::En, "Source 1")]),
    ("ui.calculations.source2", &[(Locale::En, "Source 2")]),
    ("ui.calculations.result", &[(Locale::En, "Result")]),
    ("ui.calculations.result.channel", &[(Locale::En, "New Channel")]),
    ("ui.calculations.result.selection", &[(Locale::En, "Selection")]),
    ("ui.calculations.result.document", &[(Locale::En, "New Document")]),
    ("ui.new_guide.title", &[(Locale::En, "New Guide")]),
    ("ui.new_guide.subtitle", &[(Locale::En, "Adds one guide at a document coordinate, in pixels.")]),
    ("ui.new_guide.position.must.be.finite", &[(Locale::En, "The position must be a finite number")]),
    // W10-J: View > New Guide Layout...
    ("ui.new_guide_layout.title", &[(Locale::En, "New Guide Layout")]),
    ("ui.new_guide_layout.subtitle", &[(Locale::En, "Columns and rows with gutters between them, inside optional margins, in pixels.")]),
    ("ui.new_guide_layout.clear", &[(Locale::En, "Clear existing guides")]),
    ("ui.new_guide_layout.no.room", &[(Locale::En, "The margins and gutters leave no room for a column or a row")]),
    ("ui.rename_layer.title", &[(Locale::En, "Rename Layer")]),
    // W10-A: File > Export > Slice Options...
    ("ui.slice_options.title", &[(Locale::En, "Slice Options")]),
    ("ui.slice_options.name", &[(Locale::En, "Name")]),
    ("ui.slice_options.url", &[(Locale::En, "URL")]),
    ("ui.slice_options.alt", &[(Locale::En, "Alt Tag")]),
    ("ui.slice_options.caption", &[(Locale::En, "The name is the file File > Export > Slices writes this slice as. The URL and Alt Tag go into the HTML page it writes beside the images.")]),
    ("ui.slice_options.confirm", &[(Locale::En, "OK")]),
    ("ui.slice_options.name.empty", &[(Locale::En, "A slice needs a name")]),
    ("ui.slice_options.name.taken", &[(Locale::En, "Another slice already has this name")]),
    ("ui.rename_layer.name.empty", &[(Locale::En, "The name cannot be empty")]),
    ("ui.rename_layer.name.unchanged", &[(Locale::En, "The name has not changed")]),
    // W9-K: Layer > Text > Warp Text...
    ("ui.warp_text.title", &[(Locale::En, "Warp Text")]),
    ("ui.warp_text.confirm", &[(Locale::En, "Warp")]),
    ("ui.warp_text.style", &[(Locale::En, "Style")]),
    ("ui.warp_text.bend", &[(Locale::En, "Bend")]),
    ("ui.warp_text.horizontal", &[(Locale::En, "Horizontal Distortion")]),
    ("ui.warp_text.vertical", &[(Locale::En, "Vertical Distortion")]),
    ("ui.warp_text.unchanged", &[(Locale::En, "The warp has not changed")]),
    // W13X-5: the Custom style's mesh is edited on the canvas.
    (
        "ui.warp_text.custom_hint",
        &[(
            Locale::En,
            "Custom: with the Move tool, drag the 16 mesh handles on the canvas",
        )],
    ),
    ("ui.duplicate_layer.title", &[(Locale::En, "Duplicate Layer")]),
    ("ui.duplicate_layer.name.empty", &[(Locale::En, "The name cannot be empty")]),
    ("ui.docks.show.every.layer", &[(Locale::En, "Show every layer")]),
    ("ui.docks.thumbnail.size", &[(Locale::En, "Thumbnail size")]),
    ("ui.docks.link.selected.layers", &[(Locale::En, "Link selected layers")]),
    ("ui.docks.open.the.adjustments.panel", &[(Locale::En, "Open the Adjustments panel")]),
    ("ui.docks.blending.options", &[(Locale::En, "Blending options")]),
    ("ui.docks.select.a.layer.first", &[(Locale::En, "Select a layer first")]),
    ("ui.docks.add.a.layer.mask", &[(Locale::En, "Add a layer mask")]),
    ("ui.docks.new.layer", &[(Locale::En, "New layer")]),
    ("ui.docks.new.group", &[(Locale::En, "New group")]),
    ("ui.docks.delete.selected.layers", &[(Locale::En, "Delete selected layers")]),
    ("ui.docks.mark.this.state.so.you.can", &[(Locale::En, "Mark this state so you can come back to it")]),
    ("ui.docks.the.steps.this.snapshot.named.have", &[(Locale::En, "The steps this snapshot named have been discarded")]),
    ("ui.docks.add.an.adjustment.layer", &[(Locale::En, "Add an adjustment layer")]),
    ("ui.docks.select.a.layer.to.see.its", &[(Locale::En, "Select a layer to see its properties")]),
    ("ui.docks.type.is.edited.in.character.and", &[(Locale::En, "Type is edited in Character and Paragraph")]),
    ("ui.docks.path.editing.lives.in.the.paths", &[(Locale::En, "Path editing lives in the Paths panel")]),
    ("ui.docks.clip.to.layer.below", &[(Locale::En, "Clip to layer below")]),
    ("ui.docks.this.layer.has.no.mask", &[(Locale::En, "This layer has no mask")]),
    ("ui.docks.invert.coverage", &[(Locale::En, "Invert coverage")]),
    ("ui.docks.apply.this.mask", &[(Locale::En, "Apply this mask")]),
    ("ui.docks.move.with.the.layer", &[(Locale::En, "Move with the layer")]),
    ("ui.docks.this.adjustment.has.no.panel.controls", &[(Locale::En, "This adjustment has no panel controls")]),
    ("ui.docks.invert.has.no.parameters", &[(Locale::En, "Invert has no parameters")]),
    ("ui.docks.open.editor", &[(Locale::En, "Open editor…")]),
    ("ui.docks.sample.a.colour.from.the.canvas", &[(Locale::En, "Sample a colour from the canvas")]),
    ("ui.docks.out.of.gamut", &[(Locale::En, "Out of gamut")]),
    ("ui.docks.add.current.colour", &[(Locale::En, "Add current colour")]),
    ("ui.docks.right.click.a.swatch.to.remove", &[(Locale::En, "Right-click a swatch to remove it")]),
    ("ui.docks.edit.brush", &[(Locale::En, "Edit brush…")]),
    ("ui.docks.save.current.brush", &[(Locale::En, "Save current brush")]),
    ("ui.docks.auto.leading", &[(Locale::En, "Auto leading")]),
    ("ui.docks.align.pick", &[(Locale::En, "To canvas")]),
    ("ui.docks.properties.transform.toggle", &[(Locale::En, "Show or hide the position, size and alignment fields")]),
    ("ui.docks.align.left", &[(Locale::En, "Align the left edge to the canvas")]),
    ("ui.docks.align.hcenter", &[(Locale::En, "Center horizontally on the canvas")]),
    ("ui.docks.align.right", &[(Locale::En, "Align the right edge to the canvas")]),
    ("ui.docks.align.top", &[(Locale::En, "Align the top edge to the canvas")]),
    ("ui.docks.align.vcenter", &[(Locale::En, "Center vertically on the canvas")]),
    ("ui.docks.align.bottom", &[(Locale::En, "Align the bottom edge to the canvas")]),
    ("ui.docks.properties.nothing.to.measure", &[(Locale::En, "Nothing to measure yet — paint or place something on this layer first")]),
    ("ui.docks.properties.position.locked", &[(Locale::En, "Position is locked — unlock it in the Layers panel to move or resize")]),
    ("ui.docks.shape.filled", &[(Locale::En, "Paint the inside of the path")]),
    ("ui.docks.shape.stroked", &[(Locale::En, "Outline the path")]),
    ("ui.docks.shape.no.radius", &[(Locale::En, "Corner radius applies to rectangles; this path has no corners to round")]),
    ("ui.docks.shape.radius", &[(Locale::En, "Corner radius")]),
    ("ui.docks.shape.fill.type", &[(Locale::En, "Fill type")]),
    ("ui.docks.shape.fill.colour", &[(Locale::En, "Colour")]),
    ("ui.docks.shape.fill.gradient", &[(Locale::En, "Gradient")]),
    ("ui.docks.shape.pattern", &[(Locale::En, "Pattern")]),
    ("ui.docks.shape.align", &[(Locale::En, "Align")]),
    ("ui.docks.shape.align.inside", &[(Locale::En, "Inside")]),
    ("ui.docks.shape.align.centre", &[(Locale::En, "Centre")]),
    ("ui.docks.shape.align.outside", &[(Locale::En, "Outside")]),
    ("ui.docks.shape.caps", &[(Locale::En, "Caps")]),
    ("ui.docks.shape.cap.butt", &[(Locale::En, "Butt")]),
    ("ui.docks.shape.cap.round", &[(Locale::En, "Round")]),
    ("ui.docks.shape.cap.square", &[(Locale::En, "Square")]),
    ("ui.docks.shape.corners", &[(Locale::En, "Corners")]),
    ("ui.docks.shape.join.miter", &[(Locale::En, "Miter")]),
    ("ui.docks.shape.join.round", &[(Locale::En, "Round")]),
    ("ui.docks.shape.join.bevel", &[(Locale::En, "Bevel")]),
    ("ui.docks.shape.dash", &[(Locale::En, "Dash")]),
    // W16-G: the shape page's Live Shape section (Photopea's words).
    ("ui.docks.shape.live", &[(Locale::En, "Live Shape")]),
    ("ui.docks.shape.live.w", &[(Locale::En, "W")]),
    ("ui.docks.shape.live.h", &[(Locale::En, "H")]),
    ("ui.docks.shape.live.x", &[(Locale::En, "X")]),
    ("ui.docks.shape.live.y", &[(Locale::En, "Y")]),
    ("ui.docks.shape.live.same.radii", &[(Locale::En, "Same Radii")]),
    ("ui.docks.shape.live.radius.tl", &[(Locale::En, "Top Left")]),
    ("ui.docks.shape.live.radius.tr", &[(Locale::En, "Top Right")]),
    ("ui.docks.shape.live.radius.br", &[(Locale::En, "Bottom Right")]),
    ("ui.docks.shape.live.radius.bl", &[(Locale::En, "Bottom Left")]),
    ("ui.docks.shape.live.sides", &[(Locale::En, "Sides")]),
    ("ui.docks.shape.live.points", &[(Locale::En, "Points")]),
    ("ui.docks.shape.live.inner", &[(Locale::En, "Inner Radius")]),
    ("ui.docks.shape.live.weight", &[(Locale::En, "Weight")]),
    ("ui.docks.smart.embedded", &[(Locale::En, "Embedded source")]),
    ("ui.docks.smart.linked", &[(Locale::En, "Linked file")]),
    ("ui.docks.smart.no.source", &[(Locale::En, "No source recorded for this object")]),
    // W7-E: the Layers panel's smart-filter sub-rows.
    ("ui.docks.smart.filters", &[(Locale::En, "Smart Filters")]),
    ("ui.docks.smart.filter.eye", &[(Locale::En, "Show / hide this smart filter")]),
    ("ui.docks.smart.filter.edit", &[(Locale::En, "Double-click to edit this filter's settings")]),
    ("ui.docks.smart.filter.delete", &[(Locale::En, "Delete this smart filter")]),
    // W10-I: the shared smart-filter mask row and the Animation panel.
    ("ui.docks.smart.mask.thumbnail", &[(Locale::En, "Smart filter mask: click to paint on it")]),
    ("ui.docks.smart.mask.add", &[(Locale::En, "Add a smart filter mask")]),
    ("ui.docks.smart.mask.enable", &[(Locale::En, "Enable / disable the smart filter mask")]),
    ("ui.docks.smart.mask.delete", &[(Locale::En, "Delete the smart filter mask")]),
    ("ui.animation.play", &[(Locale::En, "Play")]),
    ("ui.animation.stop", &[(Locale::En, "Stop")]),
    ("ui.animation.onion", &[(Locale::En, "Onion skin")]),
    ("ui.animation.add", &[(Locale::En, "Add frame")]),
    ("ui.animation.duplicate", &[(Locale::En, "Duplicate frame")]),
    ("ui.animation.delete", &[(Locale::En, "Delete frame")]),
    ("ui.animation.no_document", &[(Locale::En, "Open a document to animate it.")]),
    ("ui.animation.no_frames", &[(Locale::En, "No frames yet. Add frame makes an _a_ layer: each one is a frame, bottom first.")]),
    ("ui.animation.ms", &[(Locale::En, " ms")]),
    // W13-L: the Animation panel's Timeline mode.
    ("ui.animation.mode.frames", &[(Locale::En, "Frames")]),
    ("ui.animation.mode.timeline", &[(Locale::En, "Timeline")]),
    ("ui.animation.fps", &[(Locale::En, " fps")]),
    ("ui.animation.length", &[(Locale::En, "Length")]),
    ("ui.animation.key.opacity", &[(Locale::En, "Opacity key")]),
    ("ui.animation.key.position", &[(Locale::En, "Position key")]),
    ("ui.animation.key.delete", &[(Locale::En, "Delete the selected keyframe")]),
    // W13X-9: scale / rotation keys and per-key interpolation.
    ("ui.animation.key.scale", &[(Locale::En, "Scale key")]),
    ("ui.animation.key.rotation", &[(Locale::En, "Rotation key")]),
    ("ui.animation.interp.linear", &[(Locale::En, "Linear")]),
    ("ui.animation.interp.ease_in", &[(Locale::En, "Ease In")]),
    ("ui.animation.interp.ease_out", &[(Locale::En, "Ease Out")]),
    ("ui.animation.interp.hold", &[(Locale::En, "Hold")]),
    ("ui.animation.no_layers", &[(Locale::En, "No layers yet: each top-level layer gets a bar on the timeline.")]),
    // W16-M: the timeline's Add Media and video rows.
    ("ui.animation.add_media", &[(Locale::En, "Add Media")]),
    ("ui.animation.add_media.tip", &[(Locale::En, "Place an image or video file (File > Place Embedded); a video becomes a video layer at the playhead")]),
    ("ui.animation.video.frames", &[(Locale::En, " frames")]),
    ("ui.animation.new_video_group", &[(Locale::En, "New Video Group")]),
    ("ui.export_as.timeline.frames", &[(Locale::En, "{format}: the timeline, {frames} frames at {fps} fps over {length} ms; each frame shows the layers at its time")]),
    ("ui.docks.layers.search", &[(Locale::En, "Search layers by name")]),
    ("ui.docks.layers.search.placeholder", &[(Locale::En, "Search layers")]),
    ("ui.docks.history.no.document", &[(Locale::En, "Open a document to see its history")]),
    ("ui.docks.properties.no.document", &[(Locale::En, "Open a document to see its properties")]),
    ("ui.docks.layers.rename.tip", &[(Locale::En, "Double-click to rename")]),
    // W16-D: the Layers panel's effects list and panel options.
    ("ui.docks.layers.effects", &[(Locale::En, "Effects")]),
    ("ui.docks.layers.effects.eye", &[(Locale::En, "Show / hide the layer style")]),
    ("ui.docks.layers.effect.eye", &[(Locale::En, "Show / hide this effect")]),
    ("ui.docks.layers.effects.tip", &[(Locale::En, "Double-click to edit in Layer Style; drag to the trash to delete")]),
    ("ui.docks.layers.fx.toggle", &[(Locale::En, "Show / hide the effects list")]),
    ("ui.docks.layers.options", &[(Locale::En, "Layers panel options")]),
    ("ui.docks.layers.options.add.copy", &[(Locale::En, "Add \"copy\" to copied layers")]),
    ("ui.docks.layers.options.thumb.size", &[(Locale::En, "Thumbnail Size")]),
    ("ui.docks.layers.options.by.layer", &[(Locale::En, "Thumbnails by Layer")]),
    ("ui.docks.layers.options.by.document", &[(Locale::En, "Thumbnails by Document")]),
    ("ui.docks.layers.options.filter", &[(Locale::En, "Filter")]),
    ("ui.docks.layers.options.blending", &[(Locale::En, "Blending Options")]),
    ("ui.docks.layers.options.lock", &[(Locale::En, "Lock")]),
    ("ui.docks.layers.options.long.tap", &[(Locale::En, "Long-tap as a right click")]),
    ("ui.docks.character.kerning", &[(Locale::En, "Pair kerning")]),
    ("ui.docks.character.kerning.tip", &[(Locale::En, "Metrics uses the font's own pair kerning; 0 turns kerning off; Manual puts one amount (1/1000 em) between every pair of the text as it is now, and characters typed later start unkerned; it needs at least two characters, so shorter text and the Type tool defaults do not offer it. The shaper has no optical kerning, so that mode is not offered.")]),
    ("ui.docks.character.kerning.amount", &[(Locale::En, "Amount")]),
    ("ui.docks.character.ligatures", &[(Locale::En, "Standard ligatures")]),
    ("ui.docks.character.script.tip", &[(Locale::En, "Superscript raises and shrinks the text; subscript lowers and shrinks it.")]),
    ("ui.docks.character.hscale", &[(Locale::En, "Scale H")]),
    ("ui.docks.character.vscale", &[(Locale::En, "Scale V")]),
    ("ui.docks.character.hscale.tip", &[(Locale::En, "Horizontal scale, in percent: widens or narrows the glyphs and their spacing")]),
    ("ui.docks.character.vscale.tip", &[(Locale::En, "Vertical scale, in percent: stretches the glyphs about the baseline; leading is unchanged")]),
    ("ui.docks.character.baseline.shift", &[(Locale::En, "Baseline shift")]),
    ("ui.docks.character.caps.tip", &[(Locale::En, "All Caps shapes every lowercase letter as its capital; Small Caps shapes it as a capital at 70 % size. The stored text keeps the case you typed.")]),
    ("ui.docks.character.antialias.tip", &[(Locale::En, "Smooth draws grey-scale edges; None draws hard, aliased edges. The glyph scaler has one smooth mode, so Sharp, Crisp and Strong are not offered.")]),
    ("ui.docks.character.type.defaults", &[(Locale::En, "Type tool defaults")]),
    ("ui.docks.character.type.defaults.note", &[(Locale::En, "The next text layer the Type tool creates starts with this style.")]),
    ("ui.docks.paragraph.last.line", &[(Locale::En, "Last line")]),
    ("ui.docks.paragraph.indent.left", &[(Locale::En, "Left indent")]),
    ("ui.docks.paragraph.indent.right", &[(Locale::En, "Right indent")]),
    ("ui.docks.paragraph.indent.first", &[(Locale::En, "First line")]),
    ("ui.docks.character.leading.tip", &[(Locale::En, "Baseline to baseline. Auto leading follows the type size.")]),
    ("ui.docks.zoom.out", &[(Locale::En, "Zoom out")]),
    ("ui.docks.zoom.in", &[(Locale::En, "Zoom in")]),
    ("ui.docks.fit.the.whole.image.in.the", &[(Locale::En, "Fit the whole image in the window")]),




    ("ui.layer_style.drop.shadow", &[(Locale::En, "Drop Shadow")]),
    ("ui.layer_style.inner.shadow", &[(Locale::En, "Inner Shadow")]),
    ("ui.layer_style.outer.glow", &[(Locale::En, "Outer Glow")]),
    ("ui.layer_style.inner.glow", &[(Locale::En, "Inner Glow")]),
    ("ui.layer_style.color.overlay", &[(Locale::En, "Color Overlay")]),
    ("ui.layer_style.gradient.overlay", &[(Locale::En, "Gradient Overlay")]),
    ("ui.layer_style.pattern.overlay", &[(Locale::En, "Pattern Overlay")]),
    ("ui.layer_style.effects.apply.to.the.whole.layer", &[(Locale::En, "Effects apply to the whole layer and undo as one step.")]),
    ("ui.layer_style.clear.all", &[(Locale::En, "Clear All")]),
    ("ui.layer_style.styles.enabled", &[(Locale::En, "Styles enabled")]),
    ("ui.layer_style.global.light", &[(Locale::En, "Global light")]),
    ("ui.layer_style.this.effect.is.off.tick.it", &[(Locale::En, "This effect is off. Tick it in the list to edit it.")]),
    ("ui.layer_style.align.with.layer", &[(Locale::En, "Align with layer")]),
    ("ui.layer_style.edit.this.ramp", &[(Locale::En, "Edit this ramp")]),
    ("ui.layer_style.click.the.ramp.to.edit.its", &[(Locale::En, "Click the ramp to edit its stops.")]),
    ("ui.layer_style.link.with.layer", &[(Locale::En, "Link with layer")]),
    ("ui.properties.fill.edit.fill", &[(Locale::En, "Edit fill")]),
    ("ui.layer_style.approximate.the.composited.result.is.what", &[(Locale::En, "Approximate. The composited result is what the canvas shows.")]),
    ("ui.layer_style.use.global.light", &[(Locale::En, "Use global light")]),
    ("ui.layer_style.layer.knocks.out.drop.shadow", &[(Locale::En, "Layer knocks out drop shadow")]),
    ("ui.layer_style.layer.style", &[(Locale::En, "Layer Style")]),
    ("ui.layer_style.apply.style", &[(Locale::En, "Apply Style")]),
    // W2-X: the Blending Options page.
    ("ui.layer_style.blending.options", &[(Locale::En, "Blending Options")]),
    ("ui.layer_style.blending.mode", &[(Locale::En, "Mode")]),
    ("ui.layer_style.blending.opacity", &[(Locale::En, "Opacity")]),
    ("ui.layer_style.blending.fill", &[(Locale::En, "Fill")]),
    ("ui.layer_style.blending.caption", &[(Locale::En, "Opacity scales the layer and its effects; Fill scales the layer's own pixels only.")]),
    // W9-H: Styles grid, contours, Blend If, effect instances.
    ("ui.layer_style.styles", &[(Locale::En, "Styles")]),
    ("ui.layer_style.styles.empty", &[(Locale::En, "No style presets yet. Define one with Layer > Layer Style > Define Style Preset, or add a Photoshop library with File > Open (.asl).")]),
    ("ui.layer_style.contour", &[(Locale::En, "Contour")]),
    ("ui.layer_style.contour.linear", &[(Locale::En, "Linear")]),
    ("ui.layer_style.contour.cone", &[(Locale::En, "Cone")]),
    ("ui.layer_style.contour.gaussian", &[(Locale::En, "Gaussian")]),
    ("ui.layer_style.contour.ring", &[(Locale::En, "Ring")]),
    ("ui.layer_style.contour.rounded.steps", &[(Locale::En, "Rounded Steps")]),
    ("ui.layer_style.contour.custom", &[(Locale::En, "Custom")]),
    ("ui.layer_style.blend.if", &[(Locale::En, "Blend If")]),
    ("ui.layer_style.blend.if.gray", &[(Locale::En, "Gray")]),
    ("ui.layer_style.blend.if.this.layer", &[(Locale::En, "This Layer")]),
    ("ui.layer_style.blend.if.underlying", &[(Locale::En, "Underlying Layer")]),
    ("ui.layer_style.instance.add", &[(Locale::En, "Add another instance of this effect")]),
    ("ui.layer_style.instance.remove", &[(Locale::En, "Remove this instance of the effect")]),
    ("ui.export_as.give.the.export.a.file.name", &[(Locale::En, "Give the export a file name")]),
    ("ui.export_as.enable.at.least.one.export", &[(Locale::En, "Enable at least one export")]),
    ("ui.export_as.an.export.needs.at.least.one", &[(Locale::En, "An export needs at least one output")]),
    ("ui.export_as.every.enabled.row.is.written.when", &[(Locale::En, "Every enabled row is written when you export.")]),
    ("ui.export_as.file.name", &[(Locale::En, "File name")]),
    ("ui.export_as.live.preview", &[(Locale::En, "Live preview")]),
    ("ui.export_as.this.format.could.not.be.previewed", &[(Locale::En, "This format could not be previewed.")]),
    ("ui.export_as.live.preview.is.off", &[(Locale::En, "Live preview is off.")]),
    ("ui.export_as.add.export", &[(Locale::En, "Add export")]),
    ("ui.export_as.remove.export", &[(Locale::En, "Remove export")]),
    ("ui.export_as.no.export.selected", &[(Locale::En, "No export selected")]),
    ("ui.export_as.this.format.stores.8.bits.per", &[(Locale::En, "This format stores 8 bits per channel")]),
    ("ui.export_as.embed.colour.profile", &[(Locale::En, "Embed colour profile")]),
    ("ui.export_as.embed.exif.and.xmp", &[(Locale::En, "Embed EXIF and XMP")]),
    ("ui.export_as.webp.lossless", &[(Locale::En, "WebP (lossless)")]),
    ("ui.export_as.webp.lossy", &[(Locale::En, "WebP (lossy)")]),
    // W15-B: the MP4 row's Codec field.
    ("ui.export_as.codec", &[(Locale::En, "Codec")]),
    ("ui.export_as.codec.h264", &[(Locale::En, "H.264 (plays everywhere)")]),
    ("ui.export_as.codec.av1", &[(Locale::En, "AV1 (smaller, newer players)")]),
    ("ui.export_as.export.as", &[(Locale::En, "Export As")]),
    ("ui.brush_editor.custom.brush", &[(Locale::En, "Custom Brush")]),
    ("ui.brush_editor.the.preview.runs.the.real.brush", &[(Locale::En, "The preview runs the real brush engine.")]),
    ("ui.brush_editor.aliased.pencil", &[(Locale::En, "Aliased (pencil)")]),
    ("ui.brush_editor.pressure.controls.size", &[(Locale::En, "Pressure controls size")]),
    ("ui.brush_editor.pressure.controls.flow", &[(Locale::En, "Pressure controls flow")]),
    ("ui.brush_editor.min.size", &[(Locale::En, "Min size")]),
    ("ui.brush_editor.minimum.size.only.applies.when.pressure", &[(Locale::En, "Minimum size only applies when pressure controls size.")]),
    ("ui.brush_editor.brush.editor", &[(Locale::En, "Brush Editor")]),
    ("ui.brush_editor.save.brush", &[(Locale::En, "Save Brush")]),
    ("ui.brush_editor.give.the.brush.a.name", &[(Locale::En, "Give the brush a name")]),
    // W9-E: the brush editor's Tip / dynamics sections.
    ("ui.brush_editor.tip.round", &[(Locale::En, "Round (computed)")]),
    ("ui.brush_editor.tip.sampled", &[(Locale::En, "Sampled")]),
    ("ui.brush_editor.tip.missing", &[(Locale::En, "Sampled tip not loaded; painting round")]),
    ("ui.brush_editor.shape.dynamics", &[(Locale::En, "Shape Dynamics")]),
    ("ui.brush_editor.size.jitter", &[(Locale::En, "Size jitter")]),
    ("ui.brush_editor.min.diameter", &[(Locale::En, "Min diameter")]),
    ("ui.brush_editor.angle.jitter", &[(Locale::En, "Angle jitter")]),
    ("ui.brush_editor.roundness.jitter", &[(Locale::En, "Roundness jitter")]),
    ("ui.brush_editor.min.roundness", &[(Locale::En, "Min roundness")]),
    ("ui.brush_editor.both.axes", &[(Locale::En, "Both axes")]),
    ("ui.brush_editor.count.jitter", &[(Locale::En, "Count jitter")]),
    ("ui.brush_editor.color.dynamics", &[(Locale::En, "Color Dynamics")]),
    ("ui.brush_editor.fg.bg.jitter", &[(Locale::En, "Foreground/background jitter")]),
    ("ui.brush_editor.hue.jitter", &[(Locale::En, "Hue jitter")]),
    ("ui.brush_editor.saturation.jitter", &[(Locale::En, "Saturation jitter")]),
    ("ui.brush_editor.brightness.jitter", &[(Locale::En, "Brightness jitter")]),
    ("ui.brush_editor.colour.varies.per.stroke", &[(Locale::En, "Colour varies once per stroke.")]),
    ("ui.brush_editor.opacity.jitter", &[(Locale::En, "Opacity jitter")]),
    ("ui.brush_editor.flow.jitter", &[(Locale::En, "Flow jitter")]),
    ("ui.gradient_editor.spectrum", &[(Locale::En, "Spectrum")]),
    ("ui.gradient_editor.sunset", &[(Locale::En, "Sunset")]),
    ("ui.gradient_editor.copper", &[(Locale::En, "Copper")]),
    ("ui.gradient_editor.black.to.white", &[(Locale::En, "Black to White")]),
    ("ui.gradient_editor.white.to.black", &[(Locale::En, "White to Black")]),
    ("ui.gradient_editor.black.to.transparent", &[(Locale::En, "Black to Transparent")]),
    ("ui.gradient_editor.opacity.stops.sit.above.the.bar", &[(Locale::En, "Opacity stops sit above the bar, colour stops below it.")]),
    ("ui.gradient_editor.no.stop.selected", &[(Locale::En, "No stop selected")]),
    ("ui.gradient_editor.the.last.stop.has.no.segment", &[(Locale::En, "The last stop has no segment after it.")]),
    ("ui.gradient_editor.delete.stop", &[(Locale::En, "Delete stop")]),
    ("ui.gradient_editor.gradient.editor", &[(Locale::En, "Gradient Editor")]),
    ("ui.image_size.change.how.many.pixels.the.document", &[(Locale::En, "Change how many pixels the document has, or how large it prints.")]),
    ("ui.image_size.pixel.dimensions", &[(Locale::En, "Pixel dimensions")]),
    ("ui.image_size.turn.on.resample.to.change.the", &[(Locale::En, "Turn on Resample to change the pixel count.")]),
    ("ui.image_size.document.size", &[(Locale::En, "Document size")]),
    ("ui.image_size.constrain.proportions", &[(Locale::En, "Constrain proportions")]),
    ("ui.image_size.nearest.neighbour", &[(Locale::En, "Nearest Neighbour")]),
    ("ui.image_size.soft.and.cheap.good.for.a", &[(Locale::En, "Soft and cheap. Good for a small enlargement.")]),
    ("ui.image_size.the.balanced.default.for.photographs", &[(Locale::En, "The balanced default for photographs.")]),
    ("ui.image_size.sharpest.with.a.little.ringing.on", &[(Locale::En, "Sharpest, with a little ringing on hard edges.")]),
    ("ui.image_size.image.size", &[(Locale::En, "Image Size")]),
    ("ui.image_size.width.and.height.must.be.at", &[(Locale::En, "Width and height must be at least 1 pixel")]),
    ("ui.image_size.resolution.must.be.greater.than.zero", &[(Locale::En, "Resolution must be greater than zero")]),
    ("ui.canvas_size.top.left", &[(Locale::En, "Top left")]),
    ("ui.canvas_size.top.right", &[(Locale::En, "Top right")]),
    ("ui.canvas_size.bottom.left", &[(Locale::En, "Bottom left")]),
    ("ui.canvas_size.bottom.right", &[(Locale::En, "Bottom right")]),
    ("ui.canvas_size.add.or.remove.room.around.the", &[(Locale::En, "Add or remove room around the image. Pixels are not resampled.")]),
    ("ui.canvas_size.new.size", &[(Locale::En, "New size")]),
    ("ui.canvas_size.canvas.extension", &[(Locale::En, "Canvas extension")]),
    ("ui.canvas_size.canvas.size", &[(Locale::En, "Canvas Size")]),
    ("ui.canvas_size.resize.canvas", &[(Locale::En, "Resize Canvas")]),
    ("ui.canvas_size.the.canvas.must.be.at.least", &[(Locale::En, "The canvas must be at least 1 x 1 pixel")]),
    ("ui.mod.lock.transparent.pixels", &[(Locale::En, "Lock transparent pixels")]),
    ("ui.mod.lock.pixels", &[(Locale::En, "Lock pixels")]),
    ("ui.mod.lock.position", &[(Locale::En, "Lock position")]),
    ("ui.mod.lock.all", &[(Locale::En, "Lock all")]),
    ("ui.menu_bar.nothing.in.this.submenu.is.available", &[(Locale::En, "Nothing in this submenu is available right now")]),
    ("ui.status.unsaved.changes", &[(Locale::En, "Unsaved changes")]),
    ("ui.status.type.a.zoom.level", &[(Locale::En, "Type a zoom level")]),
    // The application chrome (`app_shell::chrome`): the tab strip, the status
    // strip and the start screen.
    ("ui.chrome.no.document", &[(Locale::En, "No document")]),
    ("ui.chrome.not.saved.yet", &[(Locale::En, "Not saved yet")]),
    ("ui.chrome.close.tab", &[(Locale::En, "Close")]),
    ("ui.chrome.more.tabs", &[(Locale::En, "More tabs")]),
    ("ui.chrome.more.readouts", &[(Locale::En, "More readouts")]),
    // XB: the shape tools' W/H label beside the pointer while dragging.
    ("ui.chrome.readout.width", &[(Locale::En, "W")]),
    ("ui.chrome.readout.height", &[(Locale::En, "H")]),
    ("ui.chrome.start.new", &[(Locale::En, "New")]),
    ("ui.chrome.start.new.hint", &[(Locale::En, "A blank canvas at any size")]),
    ("ui.chrome.start.open", &[(Locale::En, "Open\u{2026}")]),
    ("ui.chrome.start.open.hint", &[(Locale::En, "An image or project from this computer")]),
    ("ui.chrome.start.templates", &[(Locale::En, "Templates")]),
    ("ui.chrome.start.templates.hint", &[(Locale::En, "Screen, print and social presets")]),
    ("ui.chrome.start.recent", &[(Locale::En, "Recent")]),
    ("ui.chrome.start.no.recent", &[(Locale::En, "No recent files yet")]),
    ("ui.toolbar.swap.foreground.and.background.x", &[(Locale::En, "Swap foreground and background (X)")]),
    ("ui.toolbar.default.colours.d", &[(Locale::En, "Default colours (D)")]),
    ("ui.toolbar.this.tool.has.no.options", &[(Locale::En, "This tool has no options")]),
    ("ui.toolbar.this.tool.is.already.at.its", &[(Locale::En, "This tool is already at its defaults")]),
    ("ui.toolbar.return.this.tool.to.its.defaults", &[(Locale::En, "Return this tool to its defaults")]),
    ("ui.toolbar.straighten.layer", &[(Locale::En, "Straighten Layer")]),
    ("ui.toolbar.straighten.layer.hint", &[(Locale::En, "Rotate the active layer so the measured line is level (Enter)")]),
    ("ui.toolbar.straighten.layer.nothing", &[(Locale::En, "Drag a line with the Ruler first")]),
    // W16-C: the options bar's float units and its Commit button.
    ("ui.toolbar.unit.percent", &[(Locale::En, "%")]),
    ("ui.toolbar.unit.px", &[(Locale::En, " px")]),
    ("ui.toolbar.unit.degrees", &[(Locale::En, "\u{00B0}")]),
    ("ui.toolbar.commit.hint", &[(Locale::En, "Commit (Enter)")]),
    ("ui.toolbar.swap.colours.x", &[(Locale::En, "Swap colours  (X)")]),
    ("ui.toolbar.default.colours.d.2", &[(Locale::En, "Default colours  (D)")]),
    // W9-L: the Move bar's Align / Distribute captions and Free Transform's
    // reference-point cells.
    ("ui.toolbar.align", &[(Locale::En, "Align")]),
    ("ui.toolbar.distribute", &[(Locale::En, "Distribute")]),
    // W16-F: Path Select's Arrange / Delete buttons.
    ("ui.toolbar.path.arrange", &[(Locale::En, "Arrange")]),
    (
        "ui.toolbar.path.bring.to.front",
        &[(Locale::En, "Bring to Front")],
    ),
    (
        "ui.toolbar.path.bring.forward",
        &[(Locale::En, "Bring Forward")],
    ),
    (
        "ui.toolbar.path.send.backward",
        &[(Locale::En, "Send Backward")],
    ),
    ("ui.toolbar.path.send.to.back", &[(Locale::En, "Send to Back")]),
    ("ui.toolbar.path.delete", &[(Locale::En, "Delete")]),
    ("ui.toolbar.reference.top.left", &[(Locale::En, "Reference point: Top Left")]),
    ("ui.toolbar.reference.top", &[(Locale::En, "Reference point: Top")]),
    ("ui.toolbar.reference.top.right", &[(Locale::En, "Reference point: Top Right")]),
    ("ui.toolbar.reference.left", &[(Locale::En, "Reference point: Left")]),
    ("ui.toolbar.reference.centre", &[(Locale::En, "Reference point: Centre")]),
    ("ui.toolbar.reference.right", &[(Locale::En, "Reference point: Right")]),
    ("ui.toolbar.reference.bottom.left", &[(Locale::En, "Reference point: Bottom Left")]),
    ("ui.toolbar.reference.bottom", &[(Locale::En, "Reference point: Bottom")]),
    ("ui.toolbar.reference.bottom.right", &[(Locale::En, "Reference point: Bottom Right")]),
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
    // W1-H: the Image > Adjustments dialog.
    ("ui.adjustment.confirm", &[(Locale::En, "Apply")]),
    ("ui.adjustment.reset", &[(Locale::En, "Reset")]),
    ("ui.adjustment.preview", &[(Locale::En, "Preview")]),
    ("ui.adjustment.preview.off", &[(Locale::En, "Preview is off.")]),
    ("ui.adjustment.nothing.to.preview", &[(Locale::En, "Nothing to preview.")]),
    ("ui.adjustment.subtitle", &[(Locale::En, "Previewed on the active layer; applied to its pixels as one undoable step.")]),
    ("ui.adjustment.blocked.identity", &[(Locale::En, "Move a control first: these settings would change nothing")]),
    ("ui.adjustment.no.settings", &[(Locale::En, "This adjustment has no settings.")]),
    ("ui.adjustment.histogram", &[(Locale::En, "Luminance of the preview source; the markers are the black and white points.")]),
    ("ui.adjustment.brightness", &[(Locale::En, "Brightness")]),
    ("ui.adjustment.contrast", &[(Locale::En, "Contrast")]),
    ("ui.adjustment.black", &[(Locale::En, "Black")]),
    ("ui.adjustment.white", &[(Locale::En, "White")]),
    ("ui.adjustment.gamma", &[(Locale::En, "Gamma")]),
    ("ui.adjustment.exposure", &[(Locale::En, "Exposure")]),
    ("ui.adjustment.vibrance", &[(Locale::En, "Vibrance")]),
    ("ui.adjustment.saturation", &[(Locale::En, "Saturation")]),
    ("ui.adjustment.hue", &[(Locale::En, "Hue")]),
    ("ui.adjustment.lightness", &[(Locale::En, "Lightness")]),
    ("ui.adjustment.levels", &[(Locale::En, "Levels")]),
    ("ui.adjustment.level", &[(Locale::En, "Level")]),
    ("ui.adjustment.tone", &[(Locale::En, "Tone")]),
    ("ui.adjustment.shadows", &[(Locale::En, "Shadows")]),
    ("ui.adjustment.midtones", &[(Locale::En, "Midtones")]),
    ("ui.adjustment.highlights", &[(Locale::En, "Highlights")]),
    ("ui.adjustment.cyan.red", &[(Locale::En, "Cyan / Red")]),
    ("ui.adjustment.magenta.green", &[(Locale::En, "Magenta / Green")]),
    ("ui.adjustment.yellow.blue", &[(Locale::En, "Yellow / Blue")]),
    ("ui.adjustment.preserve.luminosity", &[(Locale::En, "Preserve luminosity")]),
    ("ui.adjustment.reds", &[(Locale::En, "Reds")]),
    ("ui.adjustment.yellows", &[(Locale::En, "Yellows")]),
    ("ui.adjustment.greens", &[(Locale::En, "Greens")]),
    ("ui.adjustment.cyans", &[(Locale::En, "Cyans")]),
    ("ui.adjustment.blues", &[(Locale::En, "Blues")]),
    ("ui.adjustment.magentas", &[(Locale::En, "Magentas")]),
    ("ui.adjustment.whites", &[(Locale::En, "Whites")]),
    ("ui.adjustment.neutrals", &[(Locale::En, "Neutrals")]),
    ("ui.adjustment.blacks", &[(Locale::En, "Blacks")]),
    ("ui.adjustment.tint", &[(Locale::En, "Tint")]),
    ("ui.adjustment.tint.hue", &[(Locale::En, "Tint hue")]),
    ("ui.adjustment.tint.saturation", &[(Locale::En, "Tint saturation")]),
    ("ui.adjustment.color", &[(Locale::En, "Color")]),
    ("ui.adjustment.density", &[(Locale::En, "Density")]),
    ("ui.adjustment.output.channel", &[(Locale::En, "Output channel")]),
    ("ui.adjustment.red", &[(Locale::En, "Red")]),
    ("ui.adjustment.green", &[(Locale::En, "Green")]),
    ("ui.adjustment.blue", &[(Locale::En, "Blue")]),
    ("ui.adjustment.constant", &[(Locale::En, "Constant")]),
    ("ui.adjustment.monochrome", &[(Locale::En, "Monochrome")]),
    ("ui.adjustment.reverse", &[(Locale::En, "Reverse")]),
    ("ui.adjustment.stop", &[(Locale::En, "Stop")]),
    ("ui.adjustment.colors", &[(Locale::En, "Colors")]),
    ("ui.adjustment.cyan", &[(Locale::En, "Cyan")]),
    ("ui.adjustment.magenta", &[(Locale::En, "Magenta")]),
    ("ui.adjustment.yellow", &[(Locale::En, "Yellow")]),
    ("ui.adjustment.black.ink", &[(Locale::En, "Black")]),
    ("ui.adjustment.relative", &[(Locale::En, "Relative")]),
    ("ui.adjustment.curve.channel", &[(Locale::En, "Channel")]),
    ("ui.adjustment.curve.rgb", &[(Locale::En, "RGB")]),
    ("ui.adjustment.curve.hint", &[(Locale::En, "Click the graph to add a point, drag a point to bend the curve, drag it off the graph to remove it")]),
    // W4-E: Shadows/Highlights, Replace Color, Color Lookup.
    ("ui.adjustment.amount", &[(Locale::En, "Amount")]),
    ("ui.adjustment.tonal.width", &[(Locale::En, "Tonal width")]),
    ("ui.adjustment.radius", &[(Locale::En, "Radius (px)")]),
    ("ui.adjustment.sampled.color", &[(Locale::En, "Sampled colour")]),
    ("ui.adjustment.fuzziness", &[(Locale::En, "Fuzziness")]),
    ("ui.adjustment.replace.click", &[(Locale::En, "Click the preview to sample the colour to replace")]),
    ("ui.adjustment.replace.selection", &[(Locale::En, "Selection: white is replaced, black is kept.")]),
    ("ui.adjustment.lut", &[(Locale::En, "Lookup table")]),
    ("ui.adjustment.lut.none", &[(Locale::En, "None")]),
    ("ui.adjustment.lut.invert", &[(Locale::En, "Invert")]),
    ("ui.adjustment.lut.warm", &[(Locale::En, "Warm")]),
    ("ui.adjustment.lut.cool", &[(Locale::En, "Cool")]),
    ("ui.adjustment.lut.sepia", &[(Locale::En, "Sepia")]),
    ("ui.adjustment.lut.high.contrast", &[(Locale::En, "High Contrast")]),
    ("ui.adjustment.lut.load", &[(Locale::En, "Load .cube file…")]),
    ("ui.adjustment.lut.file", &[(Locale::En, "Loaded file")]),
    ("ui.adjustment.lut.using", &[(Locale::En, "Using:")]),
    ("ui.adjustment.lut.error", &[(Locale::En, "That file could not be used:")]),
    // W7-G: HDR Toning and Match Color.
    ("ui.adjustment.hdr.edge.glow", &[(Locale::En, "Edge Glow")]),
    ("ui.adjustment.strength", &[(Locale::En, "Strength")]),
    ("ui.adjustment.hdr.tone.detail", &[(Locale::En, "Tone and Detail")]),
    ("ui.adjustment.detail", &[(Locale::En, "Detail")]),
    ("ui.adjustment.hdr.advanced", &[(Locale::En, "Advanced")]),
    ("ui.adjustment.match.source", &[(Locale::En, "Source")]),
    ("ui.adjustment.match.none", &[(Locale::En, "None")]),
    ("ui.adjustment.match.merged", &[(Locale::En, "Merged")]),
    ("ui.adjustment.match.no.sources", &[(Locale::En, "Open another document, or add another pixel layer, to match its colours.")]),
    ("ui.adjustment.match.image.options", &[(Locale::En, "Image Options")]),
    ("ui.adjustment.luminance", &[(Locale::En, "Luminance")]),
    ("ui.adjustment.color.intensity", &[(Locale::En, "Color Intensity")]),
    ("ui.adjustment.fade", &[(Locale::En, "Fade")]),
    ("ui.adjustment.neutralize", &[(Locale::En, "Neutralize")]),
    // W2-D: the second right column, the Histogram panel, the Navigator's
    // zoom slider, the Info sample and the Channels footer.
    ("ui.docks.side.left", &[(Locale::En, "Left")]),
    ("ui.docks.side.narrow", &[(Locale::En, "Right (narrow)")]),
    ("ui.docks.side.right", &[(Locale::En, "Right (wide)")]),
    ("ui.docks.side.bottom", &[(Locale::En, "Bottom")]),
    ("ui.docks.histogram.no.composite", &[(Locale::En, "Open a document to see its histogram")]),
    ("ui.docks.histogram.waiting", &[(Locale::En, "Waiting for the composite…")]),
    ("ui.docks.histogram.empty", &[(Locale::En, "The image has no opaque pixels to count")]),
    ("ui.docks.histogram.rgb", &[(Locale::En, "RGB")]),
    ("ui.docks.histogram.luminosity", &[(Locale::En, "Luminosity")]),
    ("ui.docks.histogram.mean", &[(Locale::En, "Mean")]),
    ("ui.docks.histogram.pixels", &[(Locale::En, "Pixels")]),
    ("ui.docks.zoom.slider", &[(Locale::En, "Drag to zoom the view")]),
    ("ui.docks.channels.thumbnail", &[(Locale::En, "Channel thumbnail")]),
    ("ui.docks.channels.load.selection", &[(Locale::En, "Load channel as selection")]),
    ("ui.docks.channels.save.selection", &[(Locale::En, "Save selection as channel")]),
    ("ui.docks.channels.new", &[(Locale::En, "New channel")]),
    ("ui.docks.channels.delete", &[(Locale::En, "Delete channel")]),
    ("ui.docks.channels.no.mask.route", &[(Locale::En, "No command loads a channel as the selection in this build; the Select menu's Load Selection restores a saved selection instead")]),
    ("ui.docks.channels.no.selection", &[(Locale::En, "Make a selection first")]),
    ("ui.docks.channels.no.alpha.store", &[(Locale::En, "This build keeps channels on layers; it has no free-standing alpha channels yet")]),
    ("ui.docks.channels.not.a.mask", &[(Locale::En, "Only a mask channel can be loaded as a selection here")]),
    ("ui.docks.channels.no.document", &[(Locale::En, "No document is open")]),
    ("ui.docks.channels.saved.hint", &[(Locale::En, "A saved selection: click to open Select > Load Selection, then choose it by name there")]),
    // W10-B: an alpha row's eye opens the channel alone for painting.
    ("ui.docks.channels.alpha.edit", &[(Locale::En, "Show this alpha channel alone in grayscale and paint into it; click again to store it")]),
    ("actions.stop", &[(Locale::En, "Stop")]),
    ("actions.replay", &[(Locale::En, "Replay")]),
    // W4-I: Paths footer, Actions library, History source column.
    ("ui.docks.history.source", &[(Locale::En, "Set the History Brush to paint from this state (the marked row)")]),
    ("ui.docks.paths.work.path", &[(Locale::En, "Work Path")]),
    ("ui.docks.paths.no.path", &[(Locale::En, "Select a path first")]),
    ("ui.docks.paths.encloses.nothing", &[(Locale::En, "The path encloses no pixel of the canvas")]),
    ("ui.docks.paths.fill", &[(Locale::En, "Fill path with the foreground colour, on a new shape layer")]),
    ("ui.docks.paths.stroke", &[(Locale::En, "Stroke path with the foreground colour at the brush size, on a new shape layer")]),
    ("ui.docks.paths.load.selection", &[(Locale::En, "Load path as a selection")]),
    ("ui.docks.paths.from.selection", &[(Locale::En, "Make work path from selection")]),
    ("ui.docks.paths.new", &[(Locale::En, "New path (saves the Work Path when it is selected)")]),
    ("ui.docks.paths.delete", &[(Locale::En, "Delete path")]),
    ("ui.docks.paths.default.name", &[(Locale::En, "Path")]),
    ("ui.docks.actions.recording", &[(Locale::En, "Recording: every edit is captured until Stop")]),
    ("ui.docks.actions.show.steps", &[(Locale::En, "Show or hide the recorded steps")]),
    ("ui.docks.actions.play", &[(Locale::En, "Play selection")]),
    ("ui.docks.actions.delete", &[(Locale::En, "Delete action")]),
    ("ui.docks.actions.save", &[(Locale::En, "Save actions")]),
    ("ui.docks.actions.load", &[(Locale::En, "Load actions")]),
    // W13-E: the Actions panel's Set -> Action -> Steps tree.
    ("ui.docks.actions.sets", &[(Locale::En, "Action sets")]),
    ("ui.docks.actions.set.show", &[(Locale::En, "Show or hide the set's actions")]),
    ("ui.docks.actions.set.recording.here", &[(Locale::En, "records here")]),
    ("ui.docks.actions.step.toggle", &[(Locale::En, "Play this step (unchecked steps are passed over)")]),
    ("ui.docks.actions.step.skipped", &[(Locale::En, "skipped on play:")]),
    ("ui.docks.actions.set.new", &[(Locale::En, "New set")]),
    ("ui.docks.actions.set.default", &[(Locale::En, "Set")]),
    ("ui.docks.actions.set.rename", &[(Locale::En, "Rename set")]),
    ("ui.docks.actions.set.record", &[(Locale::En, "Record into set")]),
    ("ui.docks.actions.set.export", &[(Locale::En, "Export set as .atn")]),
    ("ui.docks.actions.set.import", &[(Locale::En, "Load .atn")]),
    ("ui.docks.actions.play.from", &[(Locale::En, "Play from step")]),
    ("ui.docks.actions.set.delete", &[(Locale::En, "Delete set and its actions")]),
    (
        "actions.hint",
        &[(
            Locale::En,
            "Record an edit, then replay the whole sequence on any document with at least as many layers.",
        )],
    ),
    // W10-E: File > Automate, Image > Variables, Export Color Lookup / PDF,
    // Vectorize Bitmap, File Info XMP.
    ("ui.batch.choose.source", &[(Locale::En, "Choose a source folder")]),
    ("ui.batch.choose.destination", &[(Locale::En, "Choose a destination folder")]),
    ("ui.batch.same.folder", &[(Locale::En, "The source and destination must be different folders")]),
    ("ui.batch.scale.range", &[(Locale::En, "The scale must be between 1% and 1000%")]),
    ("ui.batch.no.actions", &[(Locale::En, "Record an Action in the Actions panel first")]),
    ("ui.batch.choose.action", &[(Locale::En, "Choose the Action to play")]),
    ("ui.batch.title", &[(Locale::En, "Batch")]),
    ("ui.batch.convert.title", &[(Locale::En, "Convert Formats")]),
    ("ui.batch.source", &[(Locale::En, "Source")]),
    ("ui.batch.destination", &[(Locale::En, "Destination")]),
    ("ui.batch.choose", &[(Locale::En, "Choose…")]),
    ("ui.batch.subtitle", &[(Locale::En, "Plays a recorded Action on every image in the source folder and saves each result to the destination.")]),
    ("ui.batch.convert.subtitle", &[(Locale::En, "Saves every image in the source folder to the destination in another format, quality or size.")]),
    ("ui.batch.play", &[(Locale::En, "Play")]),
    ("ui.batch.folders", &[(Locale::En, "Folders")]),
    ("ui.batch.save.as", &[(Locale::En, "Save As")]),
    ("ui.batch.format", &[(Locale::En, "Format")]),
    ("ui.batch.quality", &[(Locale::En, "Quality")]),
    ("ui.batch.scale", &[(Locale::En, "Scale %")]),
    ("ui.batch.log.note", &[(Locale::En, "Files that fail are listed in batch-errors.txt in the destination.")]),
    ("ui.batch.run", &[(Locale::En, "Run")]),
    ("ui.variables.csv.unclosed", &[(Locale::En, "The CSV ends inside a quoted field")]),
    ("ui.variables.csv.empty", &[(Locale::En, "The CSV is empty")]),
    ("ui.variables.csv.unknown", &[(Locale::En, "The CSV names a variable that is not defined:")]),
    ("ui.variables.csv.no.rows", &[(Locale::En, "The CSV has a header but no data sets")]),
    ("ui.variables.csv.row.width", &[(Locale::En, "A CSV row has the wrong number of fields, at line")]),
    ("ui.variables.set", &[(Locale::En, "Data Set")]),
    ("ui.variables.define.title", &[(Locale::En, "Variables")]),
    ("ui.variables.sets.title", &[(Locale::En, "Data Sets")]),
    ("ui.variables.name.empty", &[(Locale::En, "Every variable needs a name")]),
    ("ui.variables.name.twice", &[(Locale::En, "Two variables share a name")]),
    ("ui.variables.no.sets", &[(Locale::En, "Import a CSV of data sets first")]),
    ("ui.variables.define", &[(Locale::En, "Define")]),
    ("ui.variables.sets", &[(Locale::En, "Data Sets")]),
    ("ui.variables.ok", &[(Locale::En, "OK")]),
    ("ui.variables.export", &[(Locale::En, "Export")]),
    ("ui.variables.preview", &[(Locale::En, "Preview")]),
    ("ui.variables.define.subtitle", &[(Locale::En, "Bind a text replacement variable to a text layer, or a visibility variable to any layer.")]),
    ("ui.variables.no.layers", &[(Locale::En, "The document has no layers")]),
    ("ui.variables.text", &[(Locale::En, "Text replacement")]),
    ("ui.variables.visibility", &[(Locale::En, "Visibility")]),
    ("ui.variables.sets.subtitle", &[(Locale::En, "Import a CSV: the first row names the variables, every further row is one data set.")]),
    ("ui.variables.import", &[(Locale::En, "Import CSV…")]),
    ("ui.export_lut.small", &[(Locale::En, "Small")]),
    ("ui.export_lut.medium", &[(Locale::En, "Medium")]),
    ("ui.export_lut.large", &[(Locale::En, "Large")]),
    ("ui.export_lut.title.empty", &[(Locale::En, "The table needs a name")]),
    ("ui.export_lut.title", &[(Locale::En, "Export Color Lookup")]),
    ("ui.export_lut.subtitle", &[(Locale::En, "Writes the visible adjustment layers, applied to an identity lattice, as a 3D LUT (.cube).")]),
    ("ui.export_lut.adjustments", &[(Locale::En, "Visible adjustment layers")]),
    ("ui.export_lut.name", &[(Locale::En, "Name")]),
    ("ui.export_lut.grid", &[(Locale::En, "Grid points")]),
    ("ui.export_lut.export", &[(Locale::En, "Export")]),
    ("ui.vectorize.invalid", &[(Locale::En, "Colours must be 1 to 64, the tolerance above 0 and the corner length at least 1")]),
    ("ui.vectorize.title", &[(Locale::En, "Vectorize Bitmap")]),
    ("ui.vectorize.subtitle", &[(Locale::En, "Traces the active layer's colours into shape layers, one per colour.")]),
    ("ui.vectorize.colors", &[(Locale::En, "Colours")]),
    ("ui.vectorize.tolerance", &[(Locale::En, "Curve tolerance")]),
    ("ui.vectorize.corner", &[(Locale::En, "Corner length")]),
    ("ui.vectorize.hide.source", &[(Locale::En, "Hide the source layer")]),
    ("ui.vectorize.run", &[(Locale::En, "Vectorize")]),
    ("ui.file_info.unchanged", &[(Locale::En, "Nothing has changed")]),
    ("ui.file_info.title", &[(Locale::En, "File Info")]),
    ("ui.file_info.subtitle", &[(Locale::En, "Written as XMP into PNG, JPEG and TIFF files by Export As.")]),
    ("ui.file_info.description.section", &[(Locale::En, "Description")]),
    ("ui.file_info.doc.title", &[(Locale::En, "Document Title")]),
    ("ui.file_info.author", &[(Locale::En, "Author")]),
    ("ui.file_info.description", &[(Locale::En, "Description")]),
    ("ui.file_info.keywords", &[(Locale::En, "Keywords")]),
    ("ui.file_info.keywords.note", &[(Locale::En, "Separate keywords with commas or semicolons.")]),
    ("ui.file_info.copyright", &[(Locale::En, "Copyright Notice")]),
    ("ui.file_info.document", &[(Locale::En, "Document")]),
    ("ui.file_info.ok", &[(Locale::En, "OK")]),
    ("ui.export_as.metadata.none", &[(Locale::En, "This format has no metadata slot: nothing is embedded")]),
    ("ui.export_as.metadata.xmp.exif", &[(Locale::En, "JPEG carries the XMP and the source's EXIF")]),
    ("ui.export_as.metadata.xmp", &[(Locale::En, "Carries the XMP; the source's EXIF goes into JPEG only")]),
    ("ui.export_as.metadata.empty", &[(Locale::En, "Fill in File Info, or open a file that has EXIF, to embed metadata")]),
    ("ui.export_pdf.page.image", &[(Locale::En, "Image size")]),
    ("ui.export_pdf.page.letter", &[(Locale::En, "US Letter")]),
    ("ui.export_pdf.ppi.range", &[(Locale::En, "The resolution must be 1 to 2400 ppi")]),
    ("ui.export_pdf.title", &[(Locale::En, "Export PDF")]),
    ("ui.export_pdf.subtitle", &[(Locale::En, "One page holding the flattened image; no vector content.")]),
    ("ui.export_pdf.page", &[(Locale::En, "Page")]),
    ("ui.export_pdf.resolution", &[(Locale::En, "Resolution")]),
    ("ui.export_pdf.landscape", &[(Locale::En, "Landscape")]),
    ("ui.export_pdf.export", &[(Locale::En, "Export")]),
];

/// Keys that must resolve. The tests walk this list, so a table row whose key
/// drifted is caught next to the constant that drifted. Test-only today: the
/// moment a second locale lands, the preferences UI reads this list too.
#[cfg(test)]
const KNOWN_KEYS: &[&str] = &[
    // W13X-4.
    "ui.spot_channel.title",
    "ui.spot_channel.subtitle",
    "ui.spot_channel.name",
    "ui.spot_channel.ink",
    "ui.spot_channel.solidity",
    "ui.spot_channel.ok",
    "ui.spot_channel.no_name",
    "ui.docks.channels.menu.new.spot",
    "ui.docks.channels.menu.merge",
    "ui.docks.channels.menu.no.document",
    "ui.w16.menu.open.aco",
    "ui.w16.menu.open.abr",
    "ui.w16.menu.open.asl",
    "ui.w16.menu.export.aco",
    "ui.w16.menu.export.abr",
    "ui.w16.menu.export.asl",
    "ui.w16.menu.nothing.selected",
    "ui.w16.menu.library.empty",
    "ui.w16.menu.rename",
    "ui.w16.menu.delete",
    "ui.w16.menu.tiles.list",
    "ui.w16.menu.define.new",
    "ui.w16.menu.new.folder",
    "ui.w16.swatches.new.folder",
    "ui.w16.swatches.folder.toggle",
    "ui.w16.history.no.document",
    "ui.w16.history.nothing.to.clear",
    "ui.w16.history.clear",
    "ui.w16.history.new.snapshot",
    "ui.w16.channels.gone",
    "ui.w16.channels.color.not.deletable",
    "ui.w16.channels.menu.new",
    "ui.w16.channels.menu.delete",
    "ui.w16.channels.spot.delete",
    "ui.w16.channels.spot.options",
    "ui.w16.mask.delete",
    "ui.w16.mask.apply",
    "ui.w16.vector.mask.disable",
    "ui.w16.vector.mask.enable",
    "ui.w16.vector.mask.delete",
    "ui.w16.navigator.angle",
    "ui.w16.navigator.degrees",
    // W16-K.
    "ui.w16k.bar.pixel_to_pixel",
    "ui.w16k.bar.fit_the_area",
    "ui.w16k.bar.reset",
    "ui.w16k.crop_by.all_layers",
    "ui.w16k.crop_by.current_layer",
    "ui.w16k.crop_by.trim",
    "ui.w16k.crop_by.selection",
    "ui.w16.comps.last.state",
    "ui.w16.comps.last.state.none",
    "ui.w16.comps.flag.visibility",
    "ui.w16.comps.flag.position",
    "ui.w16.comps.flag.appearance",
    "ui.w16.notes.author",
    "ui.docks.channels.spot.hint",
    // W13X-7.
    "ui.pdf_import.title",
    "ui.pdf_import.subtitle",
    "ui.pdf_import.file",
    "ui.pdf_import.listed",
    "ui.pdf_import.pages",
    "ui.pdf_import.page",
    "ui.pdf_import.resolution",
    "ui.pdf_import.dpi",
    "ui.pdf_import.size",
    "ui.pdf_import.open_as",
    "ui.pdf_import.mode.artboards",
    "ui.pdf_import.mode.separate",
    "ui.pdf_import.none",
    "ui.pdf_import.dpi_range",
    "ui.pdf_import.ok",
    "ui.pdf_import.all",
    "ui.pdf_import.none_button",
    // W13X-3.
    "ui.scale_effects.menu",
    "ui.scale_effects.title",
    "ui.scale_effects.subtitle",
    "ui.scale_effects.scale",
    "ui.scale_effects.percent_sign",
    "ui.scale_effects.preview",
    "ui.scale_effects.rendering",
    "ui.scale_effects.ok",
    "ui.scale_effects.unchanged",
    // W13-F.
    "ui.w13f.profile.srgb",
    "ui.w13f.profile.adobe_rgb",
    "ui.w13f.profile.display_p3",
    "ui.w13f.profile.prophoto",
    "ui.w13f.profile.from_file",
    "ui.w13f.menu.assign_profile",
    "ui.w13f.menu.convert_to_profile",
    "ui.w13f.menu.reduce_colors",
    "ui.w13f.menu.wavelet",
    "ui.w13f.menu.clear_slices",
    "ui.w13f.menu.slices_from_guides",
    "ui.w13f.menu.pattern_preview",
    "ui.w13f.why.profile_already",
    "ui.w13f.why.wavelet_needs_srgb",
    "ui.w13f.why.assign_needs_rgb",
    "ui.w13f.why.convert_needs_rgb",
    "ui.w13f.why.convert_depth",
    "ui.w13f.why.needs_rgb",
    "ui.w13f.why.needs_8bit",
    "ui.w13f.why.no_slices",
    "ui.w13f.why.no_guides",
    "ui.w13f.ok",
    "ui.w13f.convert.title",
    "ui.w13f.convert.subtitle",
    "ui.w13f.convert.source",
    "ui.w13f.convert.destination",
    "ui.w13f.convert.options",
    "ui.w13f.convert.intent",
    "ui.w13f.convert.bpc",
    "ui.w13f.convert.same",
    "ui.w13f.convert.intent_note",
    "ui.w13f.intent.perceptual",
    "ui.w13f.intent.saturation",
    "ui.w13f.intent.relative",
    "ui.w13f.intent.absolute",
    "ui.w13f.reduce.title",
    "ui.w13f.reduce.subtitle",
    "ui.w13f.reduce.palette",
    "ui.w13f.reduce.colors",
    "ui.w13f.reduce.dither",
    "ui.w13f.reduce.bad_count",
    "ui.w13f.wavelet.title",
    "ui.w13f.wavelet.subtitle",
    "ui.w13f.wavelet.scales",
    "ui.w13f.wavelet.bad_count",
    "ui.w13f.status.no_document",
    "ui.w13f.status.not_a_row",
    "ui.w13f.status.icc_filter",
    "ui.w13f.status.pick_profile",
    "ui.w13f.status.no_profile_file",
    "ui.w13f.status.file_error",
    "ui.w13f.status.profile_too_big",
    "ui.w13f.status.profile_not_rgb",
    "ui.w13f.status.untransformable",
    "ui.w13f.status.needs_rgb_document",
    "ui.w13f.status.already_tagged",
    "ui.w13f.status.assigned",
    "ui.w13f.status.own_profile",
    "ui.w13f.status.already_in",
    "ui.w13f.status.refused",
    "ui.w13f.status.layer_not_rewritten",
    "ui.w13f.status.converted",
    "ui.w13f.status.select_layer",
    "ui.w13f.status.layer_missing",
    "ui.w13f.status.not_pixel_layer",
    "ui.w13f.status.locked",
    "ui.w13f.status.reduce_nothing",
    "ui.w13f.status.reduced",
    "ui.w13f.status.wavelet_other_space",
    "ui.w13f.status.wavelet_empty",
    "ui.w13f.status.layers_not_added",
    "ui.w13f.status.decomposed",
    "ui.w13f.status.residual",
    "ui.w13f.status.scale",
    "ui.w13f.status.cleared_slices",
    "ui.w13f.status.no_guide_crosses",
    "ui.w13f.status.sliced",
    // W3-H: Color Range, Select ▸ Modify, Save / Load Selection.
    "ui.selection_modify.border.title",
    "ui.selection_modify.smooth.title",
    "ui.selection_modify.expand.title",
    "ui.selection_modify.contract.title",
    "ui.selection_modify.feather.title",
    "ui.selection_modify.width",
    "ui.selection_modify.sample.radius",
    "ui.selection_modify.expand.by",
    "ui.selection_modify.contract.by",
    "ui.selection_modify.feather.radius",
    "ui.selection_modify.apply",
    "ui.selection_modify.px",
    "ui.selection_modify.out.of.range",
    "ui.selection_modify.range.to",
    "ui.selection_modify.border.caption",
    "ui.selection_name.alpha",
    "ui.selection_name.save.title",
    "ui.selection_name.load.title",
    "ui.selection_name.empty",
    "ui.selection_name.taken",
    "ui.selection_name.name",
    "ui.selection_name.save.caption",
    "ui.selection_name.save",
    "ui.selection_name.op.new",
    "ui.selection_name.op.add",
    "ui.selection_name.op.subtract",
    "ui.selection_name.op.intersect",
    "ui.selection_name.op.needs.selection",
    "ui.selection_name.none.saved",
    "ui.selection_name.channel",
    "ui.selection_name.operation",
    "ui.selection_name.invert",
    "ui.selection_name.load",
    "ui.color_range.title",
    "ui.color_range.subtitle",
    "ui.color_range.view.selection",
    "ui.color_range.view.image",
    "ui.color_range.sampled.colour",
    "ui.color_range.fuzziness",
    "ui.color_range.invert",
    "ui.color_range.click.to.sample",
    "ui.color_range.click.preview",
    "ui.color_range.select",
    "ui.color_range.eyedropper",
    // W3-G: the Preferences dialog's live controls and the keymap editor.
    "ui.preferences.minutes.0.is.off",
    "ui.preferences.ui.scale",
    "ui.preferences.language",
    "ui.preferences.only.english",
    "ui.preferences.units",
    "ui.preferences.units.caption",
    "ui.preferences.scroll.wheel.zooms.instead.of.scrolling",
    "ui.preferences.scroll.wheel.caption",
    "ui.preferences.scratch.directory",
    "ui.preferences.scratch.caption",
    "ui.preferences.press.a.key",
    "ui.preferences.add.shortcut",
    "ui.preferences.remove.this.shortcut",
    "ui.preferences.changed",
    "ui.preferences.no.commands",
    "ui.preferences.reassign.anyway",
    "ui.preferences.keep.as.it.was",
    "ui.preferences.reset.all.shortcuts",
    "ui.keymap.no.such.command",
    "ui.keymap.already.used.by",
    // W2-F: About, Trim, New Guide, Rename Layer.
    "ui.about.title",
    "ui.about.tagline",
    "ui.about.third.party.notices",
    "ui.trim.subtitle",
    "ui.trim.based.on",
    "ui.trim.transparent.pixels",
    "ui.trim.top.left.color",
    "ui.trim.bottom.right.color",
    "ui.trim.trim.away",
    "ui.trim.choose.a.side",
    // W7-D: Export As's note on a non-RGB document.
    "ui.export_as.lab.as.rgb",
    "ui.export_as.cmyk.written",
    "ui.export_as.cmyk.as.rgb",
    "ui.export_as.indexed.written",
    "ui.export_as.indexed.as.rgb",
    // W7-D: Indexed Color.
    "ui.indexed.title",
    "ui.indexed.subtitle",
    "ui.indexed.palette",
    "ui.indexed.colors",
    "ui.indexed.dither",
    "ui.indexed.exact",
    "ui.indexed.web",
    "ui.indexed.uniform",
    "ui.indexed.adaptive",
    "ui.indexed.dither.none",
    "ui.indexed.dither.diffusion",
    "ui.indexed.bad.count",
    "ui.bitmap.title",
    "ui.bitmap.subtitle",
    "ui.bitmap.method",
    "ui.bitmap.method.threshold",
    "ui.bitmap.method.pattern",
    "ui.bitmap.method.diffusion",
    "ui.bitmap.method.halftone",
    "ui.bitmap.cell",
    "ui.bitmap.angle",
    "ui.bitmap.shape",
    "ui.bitmap.shape.round",
    "ui.bitmap.shape.square",
    "ui.bitmap.shape.diamond",
    "ui.bitmap.shape.line",
    "ui.bitmap.bad.cell",
    "ui.duotone.title",
    "ui.duotone.subtitle",
    "ui.duotone.type",
    "ui.duotone.type.mono",
    "ui.duotone.type.duo",
    "ui.duotone.type.tri",
    "ui.duotone.type.quad",
    "ui.duotone.ink",
    "ui.duotone.curve",
    "ui.duotone.preview",
    "ui.apply_image.title",
    "ui.apply_image.subtitle",
    "ui.apply_image.source",
    "ui.apply_image.document",
    "ui.apply_image.layer",
    "ui.apply_image.merged",
    "ui.apply_image.channel",
    "ui.apply_image.invert",
    "ui.apply_image.blending",
    "ui.apply_image.opacity",
    "ui.apply_image.preserve",
    "ui.apply_image.use.mask",
    "ui.apply_image.mask",
    "ui.apply_image.channel.rgb",
    "ui.apply_image.channel.red",
    "ui.apply_image.channel.green",
    "ui.apply_image.channel.blue",
    "ui.apply_image.channel.gray",
    "ui.apply_image.channel.transparency",
    "ui.apply_image.no.source",
    "ui.calculations.title",
    "ui.calculations.subtitle",
    "ui.calculations.source1",
    "ui.calculations.source2",
    "ui.calculations.result",
    "ui.calculations.result.channel",
    "ui.calculations.result.selection",
    "ui.calculations.result.document",
    "ui.new_guide.title",
    "ui.new_guide.subtitle",
    "ui.new_guide.position.must.be.finite",
    "ui.new_guide_layout.title",
    "ui.new_guide_layout.subtitle",
    "ui.new_guide_layout.clear",
    "ui.new_guide_layout.no.room",
    "ui.rename_layer.title",
    "ui.slice_options.title",
    "ui.slice_options.name",
    "ui.slice_options.url",
    "ui.slice_options.alt",
    "ui.slice_options.caption",
    "ui.slice_options.confirm",
    "ui.slice_options.name.empty",
    "ui.slice_options.name.taken",
    "ui.rename_layer.name.empty",
    "ui.rename_layer.name.unchanged",
    "ui.warp_text.title",
    "ui.warp_text.confirm",
    "ui.warp_text.style",
    "ui.warp_text.bend",
    "ui.warp_text.horizontal",
    "ui.warp_text.vertical",
    "ui.warp_text.unchanged",
    "ui.warp_text.custom_hint",
    "ui.duplicate_layer.title",
    "ui.duplicate_layer.name.empty",
    // W2-D: the narrow column, the Histogram, the Navigator slider, Channels.
    "ui.docks.side.left",
    "ui.docks.side.narrow",
    "ui.docks.side.right",
    "ui.docks.side.bottom",
    "ui.docks.histogram.no.composite",
    "ui.docks.histogram.waiting",
    "ui.docks.histogram.empty",
    "ui.docks.histogram.rgb",
    "ui.docks.histogram.luminosity",
    "ui.docks.histogram.mean",
    "ui.docks.histogram.pixels",
    "ui.docks.zoom.slider",
    "ui.docks.channels.thumbnail",
    "ui.docks.channels.load.selection",
    "ui.docks.channels.save.selection",
    "ui.docks.channels.new",
    "ui.docks.channels.delete",
    "ui.docks.channels.no.mask.route",
    "ui.docks.channels.no.selection",
    "ui.docks.channels.no.alpha.store",
    "ui.docks.channels.not.a.mask",
    "ui.docks.channels.no.document",
    "ui.docks.channels.saved.hint",
    // W10-B
    "ui.docks.channels.alpha.edit",
    "actions.record",
    "actions.stop",
    "actions.replay",
    "actions.hint",
    // W4-I: Paths footer, Actions library, History source column.
    "ui.docks.history.source",
    // W4-G: the Ruler's Straighten Layer button.
    "ui.toolbar.straighten.layer",
    "ui.toolbar.straighten.layer.hint",
    "ui.toolbar.straighten.layer.nothing",
    // W16-C: the options bar's float units and its Commit button.
    "ui.toolbar.unit.percent",
    "ui.toolbar.unit.px",
    "ui.toolbar.unit.degrees",
    "ui.toolbar.commit.hint",
    // W9-L: the Move bar's Align / Distribute and the reference grid.
    "ui.toolbar.align",
    "ui.toolbar.distribute",
    // W16-F: Path Select's Arrange / Delete buttons.
    "ui.toolbar.path.arrange",
    "ui.toolbar.path.bring.to.front",
    "ui.toolbar.path.bring.forward",
    "ui.toolbar.path.send.backward",
    "ui.toolbar.path.send.to.back",
    "ui.toolbar.path.delete",
    "ui.toolbar.reference.top.left",
    "ui.toolbar.reference.top",
    "ui.toolbar.reference.top.right",
    "ui.toolbar.reference.left",
    "ui.toolbar.reference.centre",
    "ui.toolbar.reference.right",
    "ui.toolbar.reference.bottom.left",
    "ui.toolbar.reference.bottom",
    "ui.toolbar.reference.bottom.right",
    "ui.docks.paths.work.path",
    "ui.docks.paths.no.path",
    "ui.docks.paths.encloses.nothing",
    "ui.docks.paths.fill",
    "ui.docks.paths.stroke",
    "ui.docks.paths.load.selection",
    "ui.docks.paths.from.selection",
    "ui.docks.paths.new",
    "ui.docks.paths.delete",
    "ui.docks.paths.default.name",
    "ui.docks.actions.recording",
    "ui.docks.actions.show.steps",
    "ui.docks.actions.play",
    "ui.docks.actions.delete",
    "ui.docks.actions.save",
    "ui.docks.actions.load",
    // W13-E: the Actions panel's set tree.
    "ui.docks.actions.sets",
    "ui.docks.actions.set.show",
    "ui.docks.actions.set.recording.here",
    "ui.docks.actions.step.toggle",
    "ui.docks.actions.step.skipped",
    "ui.docks.actions.set.new",
    "ui.docks.actions.set.default",
    "ui.docks.actions.set.rename",
    "ui.docks.actions.set.record",
    "ui.docks.actions.set.export",
    "ui.docks.actions.set.import",
    "ui.docks.actions.play.from",
    "ui.docks.actions.set.delete",
    // Card 059: the mask well's popup rows and badge tooltip.
    "ui.docks.mask.target.badge",
    "ui.docks.mask.disable",
    "ui.docks.mask.enable",
    "ui.docks.mask.toggle.link",
    "ui.docks.mask.view.composite",
    "ui.docks.mask.view.grayscale",
    "ui.docks.mask.view.overlay",
    // Card 060: the Refine Mask dialog.
    "ui.refine_mask.subtitle",
    "ui.refine_mask.feather",
    "ui.refine_mask.shift",
    "ui.refine_mask.smooth",
    "ui.refine_mask.contrast",
    "ui.refine_mask.background.label",
    "ui.refine_mask.background.black",
    "ui.refine_mask.background.white",
    "ui.refine_mask.background.checker",
    "ui.refine_mask.confirm",
    "ui.refine_mask.title",
    "ui.defringe.subtitle",
    "ui.defringe.radius",
    "ui.defringe.strength",
    "ui.defringe.confirm",
    "ui.defringe.title",
    "ui.defringe.nothing.to.clean",
    "ui.refine_mask.px.suffix",
    "ui.refine_mask.nothing.to.refine",
    // W1-H: the Image > Adjustments dialog.
    "ui.adjustment.confirm",
    // W4-E: Shadows/Highlights, Replace Color, Color Lookup.
    "ui.adjustment.amount",
    "ui.adjustment.tonal.width",
    "ui.adjustment.radius",
    "ui.adjustment.sampled.color",
    "ui.adjustment.fuzziness",
    "ui.adjustment.replace.click",
    "ui.adjustment.replace.selection",
    "ui.adjustment.lut",
    "ui.adjustment.lut.none",
    "ui.adjustment.lut.invert",
    "ui.adjustment.lut.warm",
    "ui.adjustment.lut.cool",
    "ui.adjustment.lut.sepia",
    "ui.adjustment.lut.high.contrast",
    "ui.adjustment.lut.load",
    "ui.adjustment.lut.file",
    "ui.adjustment.lut.using",
    "ui.adjustment.lut.error",
    // W7-G: HDR Toning and Match Color.
    "ui.adjustment.hdr.edge.glow",
    "ui.adjustment.strength",
    "ui.adjustment.hdr.tone.detail",
    "ui.adjustment.detail",
    "ui.adjustment.hdr.advanced",
    "ui.adjustment.match.source",
    "ui.adjustment.match.none",
    "ui.adjustment.match.merged",
    "ui.adjustment.match.no.sources",
    "ui.adjustment.match.image.options",
    "ui.adjustment.luminance",
    "ui.adjustment.color.intensity",
    "ui.adjustment.fade",
    "ui.adjustment.neutralize",
    "ui.adjustment.reset",
    "ui.adjustment.preview",
    "ui.adjustment.preview.off",
    "ui.adjustment.nothing.to.preview",
    "ui.adjustment.subtitle",
    "ui.adjustment.blocked.identity",
    "ui.adjustment.no.settings",
    "ui.adjustment.histogram",
    "ui.adjustment.brightness",
    "ui.adjustment.contrast",
    "ui.adjustment.black",
    "ui.adjustment.white",
    "ui.adjustment.gamma",
    "ui.adjustment.exposure",
    "ui.adjustment.vibrance",
    "ui.adjustment.saturation",
    "ui.adjustment.hue",
    "ui.adjustment.lightness",
    "ui.adjustment.levels",
    "ui.adjustment.level",
    "ui.adjustment.tone",
    "ui.adjustment.shadows",
    "ui.adjustment.midtones",
    "ui.adjustment.highlights",
    "ui.adjustment.cyan.red",
    "ui.adjustment.magenta.green",
    "ui.adjustment.yellow.blue",
    "ui.adjustment.preserve.luminosity",
    "ui.adjustment.reds",
    "ui.adjustment.yellows",
    "ui.adjustment.greens",
    "ui.adjustment.cyans",
    "ui.adjustment.blues",
    "ui.adjustment.magentas",
    "ui.adjustment.whites",
    "ui.adjustment.neutrals",
    "ui.adjustment.blacks",
    "ui.adjustment.tint",
    "ui.adjustment.tint.hue",
    "ui.adjustment.tint.saturation",
    "ui.adjustment.color",
    "ui.adjustment.density",
    "ui.adjustment.output.channel",
    "ui.adjustment.red",
    "ui.adjustment.green",
    "ui.adjustment.blue",
    "ui.adjustment.constant",
    "ui.adjustment.monochrome",
    "ui.adjustment.reverse",
    "ui.adjustment.stop",
    "ui.adjustment.colors",
    "ui.adjustment.cyan",
    "ui.adjustment.magenta",
    "ui.adjustment.yellow",
    "ui.adjustment.black.ink",
    "ui.adjustment.relative",
    "ui.adjustment.curve.channel",
    "ui.adjustment.curve.rgb",
    "ui.adjustment.curve.hint",
    // W2-X: the Layer Style dialog's Blending Options page.
    "ui.layer_style.blending.options",
    "ui.layer_style.blending.mode",
    "ui.layer_style.blending.opacity",
    "ui.layer_style.blending.fill",
    "ui.layer_style.blending.caption",
    // W9-H: Styles grid, contours, Blend If, effect instances.
    "ui.layer_style.styles",
    "ui.layer_style.styles.empty",
    "ui.layer_style.contour",
    "ui.layer_style.contour.linear",
    "ui.layer_style.contour.cone",
    "ui.layer_style.contour.gaussian",
    "ui.layer_style.contour.ring",
    "ui.layer_style.contour.rounded.steps",
    "ui.layer_style.contour.custom",
    "ui.layer_style.blend.if",
    "ui.layer_style.blend.if.gray",
    "ui.layer_style.blend.if.this.layer",
    "ui.layer_style.blend.if.underlying",
    "ui.layer_style.instance.add",
    "ui.layer_style.instance.remove",
    // W3-J: Properties transform / shape / smart-object pages, Layers search
    // and rename, Character kerning / ligatures / script.
    "ui.docks.align.pick",
    "ui.docks.properties.transform.toggle",
    "ui.docks.align.left",
    "ui.docks.align.hcenter",
    "ui.docks.align.right",
    "ui.docks.align.top",
    "ui.docks.align.vcenter",
    "ui.docks.align.bottom",
    "ui.docks.properties.nothing.to.measure",
    "ui.docks.properties.position.locked",
    "ui.docks.shape.filled",
    "ui.docks.shape.stroked",
    "ui.docks.shape.no.radius",
    "ui.docks.shape.radius",
    "ui.docks.shape.fill.type",
    "ui.docks.shape.fill.colour",
    "ui.docks.shape.fill.gradient",
    "ui.docks.shape.pattern",
    "ui.docks.shape.align",
    "ui.docks.shape.align.inside",
    "ui.docks.shape.align.centre",
    "ui.docks.shape.align.outside",
    "ui.docks.shape.caps",
    "ui.docks.shape.cap.butt",
    "ui.docks.shape.cap.round",
    "ui.docks.shape.cap.square",
    "ui.docks.shape.corners",
    "ui.docks.shape.join.miter",
    "ui.docks.shape.join.round",
    "ui.docks.shape.join.bevel",
    "ui.docks.shape.dash",
    // W16-G.
    "ui.docks.shape.live",
    "ui.docks.shape.live.w",
    "ui.docks.shape.live.h",
    "ui.docks.shape.live.x",
    "ui.docks.shape.live.y",
    "ui.docks.shape.live.same.radii",
    "ui.docks.shape.live.radius.tl",
    "ui.docks.shape.live.radius.tr",
    "ui.docks.shape.live.radius.br",
    "ui.docks.shape.live.radius.bl",
    "ui.docks.shape.live.sides",
    "ui.docks.shape.live.points",
    "ui.docks.shape.live.inner",
    "ui.docks.shape.live.weight",
    "ui.docks.smart.embedded",
    "ui.docks.smart.linked",
    "ui.docks.smart.no.source",
    "ui.docks.smart.filters",
    "ui.docks.smart.filter.eye",
    "ui.docks.smart.filter.edit",
    "ui.docks.smart.filter.delete",
    // W10-I: the shared smart-filter mask row and the Animation panel.
    "ui.docks.smart.mask.thumbnail",
    "ui.docks.smart.mask.add",
    "ui.docks.smart.mask.enable",
    "ui.docks.smart.mask.delete",
    "ui.animation.play",
    "ui.animation.stop",
    "ui.animation.onion",
    "ui.animation.add",
    "ui.animation.duplicate",
    "ui.animation.delete",
    "ui.animation.no_document",
    "ui.animation.no_frames",
    "ui.animation.ms",
    // W13-L: the Animation panel's Timeline mode.
    "ui.animation.mode.frames",
    "ui.animation.mode.timeline",
    "ui.animation.fps",
    "ui.animation.length",
    "ui.animation.key.opacity",
    "ui.animation.key.position",
    "ui.animation.key.delete",
    // W13X-9
    "ui.animation.key.scale",
    "ui.animation.key.rotation",
    "ui.animation.interp.linear",
    "ui.animation.interp.ease_in",
    "ui.animation.interp.ease_out",
    "ui.animation.interp.hold",
    "ui.animation.no_layers",
    // W16-M
    "ui.animation.add_media",
    "ui.animation.add_media.tip",
    "ui.animation.video.frames",
    "ui.animation.new_video_group",
    "ui.export_as.timeline.frames",
    "ui.docks.layers.search",
    "ui.docks.layers.search.placeholder",
    "ui.docks.history.no.document",
    "ui.docks.properties.no.document",
    "ui.docks.layers.rename.tip",
    // W16-D
    "ui.docks.layers.effects",
    "ui.docks.layers.effects.eye",
    "ui.docks.layers.effect.eye",
    "ui.docks.layers.effects.tip",
    "ui.docks.layers.fx.toggle",
    "ui.docks.layers.options",
    "ui.docks.layers.options.add.copy",
    "ui.docks.layers.options.thumb.size",
    "ui.docks.layers.options.by.layer",
    "ui.docks.layers.options.by.document",
    "ui.docks.layers.options.filter",
    "ui.docks.layers.options.blending",
    "ui.docks.layers.options.lock",
    "ui.docks.layers.options.long.tap",
    "ui.docks.character.kerning",
    "ui.docks.character.kerning.tip",
    "ui.docks.character.kerning.amount",
    "ui.docks.character.ligatures",
    "ui.docks.character.script.tip",
    "ui.docks.character.hscale",
    "ui.docks.character.vscale",
    "ui.docks.character.hscale.tip",
    "ui.docks.character.vscale.tip",
    "ui.docks.character.baseline.shift",
    "ui.docks.character.caps.tip",
    "ui.docks.character.antialias.tip",
    "ui.docks.character.type.defaults",
    "ui.docks.character.type.defaults.note",
    "ui.docks.paragraph.last.line",
    "ui.docks.paragraph.indent.left",
    "ui.docks.paragraph.indent.right",
    "ui.docks.paragraph.indent.first",
    "ui.docks.character.leading.tip",
    "ui.properties.fill.edit.fill",
    // W10-E: File > Automate, Image > Variables, Export Color Lookup / PDF,
    // Vectorize Bitmap, File Info XMP.
    "ui.batch.choose.source",
    "ui.batch.choose.destination",
    "ui.batch.same.folder",
    "ui.batch.scale.range",
    "ui.batch.no.actions",
    "ui.batch.choose.action",
    "ui.batch.title",
    "ui.batch.convert.title",
    "ui.batch.source",
    "ui.batch.destination",
    "ui.batch.choose",
    "ui.batch.subtitle",
    "ui.batch.convert.subtitle",
    "ui.batch.play",
    "ui.batch.folders",
    "ui.batch.save.as",
    "ui.batch.format",
    "ui.batch.quality",
    "ui.batch.scale",
    "ui.batch.log.note",
    "ui.batch.run",
    "ui.variables.csv.unclosed",
    "ui.variables.csv.empty",
    "ui.variables.csv.unknown",
    "ui.variables.csv.no.rows",
    "ui.variables.csv.row.width",
    "ui.variables.set",
    "ui.variables.define.title",
    "ui.variables.sets.title",
    "ui.variables.name.empty",
    "ui.variables.name.twice",
    "ui.variables.no.sets",
    "ui.variables.define",
    "ui.variables.sets",
    "ui.variables.ok",
    "ui.variables.export",
    "ui.variables.preview",
    "ui.variables.define.subtitle",
    "ui.variables.no.layers",
    "ui.variables.text",
    "ui.variables.visibility",
    "ui.variables.sets.subtitle",
    "ui.variables.import",
    "ui.export_lut.small",
    "ui.export_lut.medium",
    "ui.export_lut.large",
    "ui.export_lut.title.empty",
    "ui.export_lut.title",
    "ui.export_lut.subtitle",
    "ui.export_lut.adjustments",
    "ui.export_lut.name",
    "ui.export_lut.grid",
    "ui.export_lut.export",
    "ui.vectorize.invalid",
    "ui.vectorize.title",
    "ui.vectorize.subtitle",
    "ui.vectorize.colors",
    "ui.vectorize.tolerance",
    "ui.vectorize.corner",
    "ui.vectorize.hide.source",
    "ui.vectorize.run",
    "ui.file_info.unchanged",
    "ui.file_info.title",
    "ui.file_info.subtitle",
    "ui.file_info.description.section",
    "ui.file_info.doc.title",
    "ui.file_info.author",
    "ui.file_info.description",
    "ui.file_info.keywords",
    "ui.file_info.keywords.note",
    "ui.file_info.copyright",
    "ui.file_info.document",
    "ui.file_info.ok",
    "ui.export_as.metadata.none",
    "ui.export_as.metadata.xmp.exif",
    "ui.export_as.metadata.xmp",
    "ui.export_as.metadata.empty",
    "ui.export_pdf.page.image",
    "ui.export_pdf.page.letter",
    "ui.export_pdf.ppi.range",
    "ui.export_pdf.title",
    "ui.export_pdf.subtitle",
    "ui.export_pdf.page",
    "ui.export_pdf.resolution",
    "ui.export_pdf.landscape",
    "ui.export_pdf.export",
];

/// Resolve `key` in the active locale, falling back to English. An
/// unregistered key is a bug the catalogue tests catch; the tests name every
/// key the migrated modules use (KNOWN_KEYS), so a leak here means a module
/// grew a string without a table row. At runtime the empty string is better
/// than a panic or a rogue key leaking into the UI.
///
/// W16-N: a row's own entry for the locale wins; otherwise the English
/// source is looked up in that language's table ([`tr_en`]), so a string
/// shared by many keys ("OK") is translated once.
pub fn tr(key: &str) -> &'static str {
    let Some(row) = rows_by_key().get(key) else {
        return "";
    };
    let locale = active();
    if let Some((_, own)) = row.iter().find(|(l, _)| *l == locale) {
        return own;
    }
    let english = row
        .iter()
        .find(|(l, _)| *l == Locale::En)
        .map(|(_, s)| *s)
        .unwrap_or("");
    tr_en(english)
}

/// [`TABLE`] indexed by key, built once. A key listed twice keeps its first
/// row, as the linear scan this replaced did.
fn rows_by_key() -> &'static HashMap<&'static str, &'static [(Locale, &'static str)]> {
    static INDEX: OnceLock<HashMap<&'static str, &'static [(Locale, &'static str)]>> =
        OnceLock::new();
    INDEX.get_or_init(|| {
        let mut index = HashMap::with_capacity(TABLE.len());
        for (key, row) in TABLE {
            index.entry(*key).or_insert(*row);
        }
        index
    })
}

/// W16-N: every English source string the language tables must translate:
/// the catalogue's own rows plus the strings the model crates hand the UI
/// ([`crate::menu`], [`crate::dock`], the blend modes, the history steps).
/// The gate test walks it for every language; it is public so the shell's
/// own tests can prove a string they draw is in it.
pub fn catalogue_sources() -> Vec<String> {
    let mut out: Vec<String> = TABLE
        .iter()
        .filter_map(|(_, row)| row.iter().find(|(l, _)| *l == Locale::En))
        .map(|(_, s)| s.to_string())
        .collect();
    out.extend(with_locale(Locale::En, i18n_sources::sources));
    out.retain(|s| !s.is_empty() && !is_untranslatable(s));
    out.sort_unstable();
    out.dedup();
    out
}

/// A string no language translates: it has no letters (a unit, a
/// placeholder-only pattern such as `{path}: {error}`), or it is a proper
/// name the tables keep as it is (a file format, a colour standard).
fn is_untranslatable(s: &str) -> bool {
    let mut outside_braces = String::new();
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            c if depth == 0 => outside_braces.push(c),
            _ => {}
        }
    }
    !outside_braces.chars().any(char::is_alphabetic) || i18n_sources::PROPER_NAMES.contains(&s)
}

/// The English strings the model crates hand the UI (see
/// [`catalogue_sources`]).
#[path = "i18n/sources.rs"]
mod i18n_sources;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_resolves_every_registered_key_for_every_locale() {
        for (key, row) in TABLE {
            for (locale, expected) in row.iter() {
                // The first row of a key listed twice is the one that shows.
                if TABLE.iter().find(|(k, _)| k == key).map(|(_, r)| *r) != Some(*row) {
                    continue;
                }
                with_locale(*locale, || {
                    assert_eq!(tr(key), *expected, "key {key:?} in {locale:?}");
                });
            }
        }
        // English is always complete and always the fallback.
        for (key, row) in TABLE {
            assert!(
                row.iter().any(|(l, _)| *l == Locale::En),
                "{key:?} has no English entry; English is the fallback source"
            );
        }
    }

    #[test]
    fn every_listed_locale_round_trips_through_its_code_and_unknown_codes_fall_back() {
        for locale in Locale::ALL {
            assert_eq!(Locale::from_code(locale.code()), *locale);
            with_locale(*locale, || assert_eq!(active(), *locale));
        }
        assert_eq!(Locale::from_code("xx-not-a-locale"), Locale::En);
    }

    #[test]
    fn an_unknown_key_is_empty_rather_than_a_leak_or_a_panic() {
        assert_eq!(tr("not.a.key"), "");
    }

    /// W16-N: the English strings drawn through `tr_en("...")` literals in
    /// the ui and app-shell sources, so a call site added later is gated
    /// without anyone listing it by hand.
    fn tr_en_literals() -> Vec<String> {
        fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else if path.extension().is_some_and(|e| e == "rs")
                    && path.file_name().is_some_and(|n| n != "strings.rs")
                {
                    out.push(path);
                }
            }
        }
        let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut files = Vec::new();
        walk(&manifest.join("src"), &mut files);
        walk(&manifest.join("../app-shell/src"), &mut files);
        let mut out = Vec::new();
        for file in files {
            let text = std::fs::read_to_string(&file).unwrap_or_default();
            let mut rest = text.as_str();
            while let Some(at) = rest.find("tr_en(\"") {
                rest = &rest[at + "tr_en(\"".len()..];
                if let Some(end) = rest.find('"') {
                    out.push(rest[..end].to_string());
                }
            }
        }
        out
    }

    /// The strings every language table must translate.
    fn gated_sources() -> Vec<String> {
        let mut all = catalogue_sources();
        all.extend(tr_en_literals());
        all.retain(|s| !s.is_empty() && !is_untranslatable(s));
        all.sort_unstable();
        all.dedup();
        all
    }

    /// The `{name}` placeholders of a string, sorted.
    fn placeholders(s: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut rest = s;
        while let Some(open) = rest.find('{') {
            rest = &rest[open + 1..];
            let Some(close) = rest.find('}') else { break };
            out.push(rest[..close].to_string());
            rest = &rest[close + 1..];
        }
        out.sort_unstable();
        out
    }

    /// W16-N, the gate: every language offered translates every string the
    /// interface draws through the catalogue — every `tr()` key's English,
    /// every menu title and row, panel tab, blend mode, history step and the
    /// other model labels (`catalogue_sources`), and every `tr_en("...")`
    /// literal — keeps each `{placeholder}` and each menu ellipsis, and carries
    /// no row for a string nothing draws any more.
    #[test]
    fn every_language_table_translates_every_catalogue_string() {
        let sources = gated_sources();
        assert!(
            sources.len() > 1500,
            "only {} sources gathered",
            sources.len()
        );
        let mut problems = Vec::new();
        for locale in Locale::ALL.iter().copied().filter(|l| *l != Locale::En) {
            let table = catalogue(locale).expect("every non-English locale has a table");
            assert!(!table.name.is_empty(), "{locale:?} has no @name row");
            let mut missing = 0usize;
            for english in &sources {
                let Some(translated) = table.rows.get(english.as_str()) else {
                    missing += 1;
                    if missing <= 20 {
                        problems.push(format!("{locale:?} is missing {english:?}"));
                    }
                    continue;
                };
                if translated.trim().is_empty() {
                    problems.push(format!("{locale:?} translates {english:?} as nothing"));
                }
                if placeholders(english) != placeholders(translated) {
                    problems.push(format!(
                        "{locale:?}: {english:?} -> {translated:?} changes the placeholders"
                    ));
                }
                if english.ends_with('\u{2026}') != translated.ends_with('\u{2026}') {
                    problems.push(format!(
                        "{locale:?}: {english:?} -> {translated:?} changes the ellipsis"
                    ));
                }
            }
            if missing > 20 {
                problems.push(format!("{locale:?}: {missing} strings missing in all"));
            }
            for english in table.rows.keys() {
                if sources
                    .binary_search_by(|s| s.as_str().cmp(english))
                    .is_err()
                {
                    problems.push(format!("{locale:?} has a stale row {english:?}"));
                }
            }
        }
        assert!(problems.is_empty(), "{}", problems.join("\n"));
    }

    #[test]
    fn a_translated_locale_changes_what_tr_and_tr_en_return_and_english_stays_the_source() {
        with_locale(Locale::De, || {
            assert_eq!(tr_en("File"), "Datei");
            assert_eq!(tr("ui.pdf_import.ok"), tr_en("Open"));
            assert_ne!(tr("ui.pdf_import.ok"), "Open");
            // A string no table has (a file or layer name) is shown as it is.
            assert_eq!(tr_en("holiday-2026.psd"), "holiday-2026.psd");
            // A history step named after a menu row, ellipsis dropped.
            assert_eq!(
                tr_en("Gaussian Blur"),
                tr_en("Gaussian Blur\u{2026}").trim_end_matches('\u{2026}')
            );
            assert_ne!(tr_en("Gaussian Blur"), "Gaussian Blur");
        });
        with_locale(Locale::Ja, || assert_ne!(tr_en("File"), "File"));
        assert_eq!(tr_en("File"), "File");
        assert_eq!(Locale::De.display_name(), "Deutsch");
    }

    #[test]
    fn with_locale_is_scoped_to_its_thread_and_restores_after_a_panic() {
        let outer = active();
        let caught = std::panic::catch_unwind(|| {
            with_locale(Locale::Fr, || {
                assert_eq!(active(), Locale::Fr);
                let other = std::thread::spawn(active).join().unwrap();
                assert_eq!(other, Locale::En, "another thread keeps the process locale");
                panic!("unwind through the scope");
            })
        });
        assert!(caught.is_err());
        assert_eq!(active(), outer);
    }

    /// W16-N: the Chinese, Japanese and Korean tables — and every language's
    /// own name in the language list — draw as glyphs once [`install_fonts`]
    /// has run; egui's own fonts carry none of them (the anti-vacuity half).
    #[test]
    fn the_bundled_cjk_face_draws_every_cjk_row_and_egui_alone_cannot() {
        let frame = |ctx: &egui::Context| {
            let _ = ctx.run(egui::RawInput::default(), |_| {});
        };
        let body = egui::FontId::proportional(13.0);
        let mono = egui::FontId::monospace(13.0);
        let bare = egui::Context::default();
        frame(&bare);
        assert!(
            !bare.fonts(|f| f.has_glyphs(&body, "日本語")),
            "egui's own fonts already draw CJK; this test would prove nothing"
        );
        let ctx = egui::Context::default();
        install_fonts(&ctx);
        install_fonts(&ctx); // idempotent: a second call changes nothing
        frame(&ctx);
        for locale in Locale::ALL.iter().copied() {
            let name = locale.display_name();
            assert!(
                ctx.fonts(|f| f.has_glyphs(&body, name)),
                "{name:?} has tofu"
            );
        }
        for locale in [Locale::ZhCn, Locale::Ja, Locale::Ko] {
            assert!(locale.needs_cjk_font());
            let table = catalogue(locale).expect("a CJK table");
            for text in table.rows.values() {
                assert!(
                    ctx.fonts(|f| f.has_glyphs(&body, text) && f.has_glyphs(&mono, text)),
                    "{locale:?}: {text:?} has a character the fonts cannot draw"
                );
            }
        }
    }

    /// W16-N: the menu model the bar draws is in the active language — titles,
    /// submenus and rows — while `MenuAction::label`, which scripts and tests
    /// match on, stays the English source; Window carries Language (every
    /// offered language, each in its own name) and Glass Menus.
    #[test]
    fn the_menu_bar_model_speaks_the_active_language_and_offers_the_language_list() {
        use crate::menu::{menu_bar, Entry, MenuAction, MenuContext};
        let titles = || menu_bar(0).iter().map(|m| m.title).collect::<Vec<_>>();
        assert_eq!(
            titles(),
            ["File", "Edit", "Image", "Layer", "Select", "Filter", "View", "Window", "Help"]
        );
        with_locale(Locale::De, || {
            assert_eq!(
                titles(),
                [
                    "Datei",
                    "Bearbeiten",
                    "Bild",
                    "Ebene",
                    "Auswahl",
                    "Filter",
                    "Ansicht",
                    "Fenster",
                    "Hilfe"
                ]
            );
            let ctx = MenuContext::default();
            assert_eq!(
                MenuAction::Open.label(),
                "Open\u{2026}",
                "the source stays English"
            );
            assert_eq!(MenuAction::Open.label_in(&ctx), "\u{00D6}ffnen\u{2026}");
            let undo = MenuContext {
                undo_label: Some("Create Layer".into()),
                ..MenuContext::default()
            };
            assert_eq!(
                MenuAction::Undo.label_in(&undo),
                "R\u{00FC}ckg\u{00E4}ngig Ebene erstellen"
            );
            let window = menu_bar(0)
                .into_iter()
                .find(|m| m.title == "Fenster")
                .unwrap();
            let languages = window
                .entries
                .iter()
                .find_map(|e| match e {
                    Entry::Submenu { label, entries } if *label == "Sprache" => {
                        Some(entries.clone())
                    }
                    _ => None,
                })
                .expect("Window has a Language submenu");
            let offered: Vec<String> = languages
                .iter()
                .flat_map(Entry::actions)
                .map(|a| a.label_in(&ctx))
                .collect();
            let names: Vec<String> = Locale::ALL
                .iter()
                .map(|l| l.display_name().to_string())
                .collect();
            assert_eq!(offered, names, "every language, each in its own name");
            // Glass Menus closes the themes list below a separator, as it
            // closes Photopea's More > Themes.
            let appearance = window
                .entries
                .iter()
                .find_map(|e| match e {
                    Entry::Submenu { label, entries } if *label == "Erscheinungsbild" => {
                        Some(entries.clone())
                    }
                    _ => None,
                })
                .expect("Window has an Appearance submenu");
            assert!(
                matches!(
                    appearance.as_slice(),
                    [
                        ..,
                        Entry::Separator,
                        Entry::Item(MenuAction::ToggleGlassMenus)
                    ]
                ),
                "Appearance ends with a separator and Glass Menus: {appearance:?}"
            );
            assert_eq!(
                window
                    .actions()
                    .iter()
                    .filter(|a| **a == MenuAction::ToggleGlassMenus)
                    .count(),
                1,
                "Glass Menus is offered once"
            );
            assert_eq!(
                MenuAction::ToggleGlassMenus.label_in(&ctx),
                "Glasmen\u{00FC}s"
            );
            assert_eq!(
                MenuAction::SetLanguage(Locale::De).checked(&ctx),
                Some(true)
            );
            assert_eq!(
                MenuAction::SetLanguage(Locale::Fr).checked(&ctx),
                Some(false)
            );
        });
    }

    #[test]
    fn the_catalogue_parser_reads_names_rows_escapes_and_skips_comments() {
        let parsed = parse_catalogue(
            "# a comment\n@name\tTest\nA\\tB\tC\\nD\r\nplain\tsimple\n\nno tab here\n",
        );
        assert_eq!(parsed.name, "Test");
        assert_eq!(parsed.rows.get("A\tB"), Some(&"C\nD"));
        assert_eq!(parsed.rows.get("plain"), Some(&"simple"));
        assert_eq!(parsed.rows.len(), 2);
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
