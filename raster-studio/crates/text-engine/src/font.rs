//! Font discovery, loading and matching.
//!
//! [`FontLibrary`] owns the shaping stack's font database. It can be built
//! empty (deterministic — used by the tests), from the fonts installed on the
//! machine, or from raw bytes handed in by the document (embedded fonts).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use cosmic_text::fontdb::{Database, Family, Query, Source, Stretch, Style as DbStyle, Weight};
use cosmic_text::FontSystem;

use crate::style::{FontSlant, FontStretch, FontWeight};

/// Stable handle to one face in the library's database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FontId(pub(crate) cosmic_text::fontdb::ID);

/// Description of a single face, as the UI needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaceRecord {
    /// Handle for this face.
    pub id: FontId,
    /// Family name this face belongs to.
    pub family: String,
    /// `PostScript` name, unique per face.
    pub post_script_name: String,
    /// Weight of the face as the font declares it.
    pub weight: FontWeight,
    /// Slant of the face as the font declares it.
    pub slant: FontSlant,
    /// Face width as the font declares it (the `usWidthClass` step).
    pub stretch: FontStretch,
    /// Whether the face is monospaced.
    pub monospaced: bool,
}

/// A family and every face in it — one row of the UI's family picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FamilyRecord {
    /// Family name.
    pub name: String,
    /// Faces, sorted by weight then slant.
    pub faces: Vec<FaceRecord>,
}

/// The result of matching a style request against the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaceMatch {
    /// The face that will actually be used.
    pub id: FontId,
    /// The matched face is too light: the renderer must embolden it.
    pub synthetic_bold: bool,
    /// The matched face is upright: the renderer must skew it.
    pub synthetic_italic: bool,
}

/// Metrics of a face, in font design units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FaceMetrics {
    /// Design units per em.
    pub units_per_em: f32,
    /// Ascent, positive above the baseline.
    pub ascent: f32,
    /// Descent, negative below the baseline.
    pub descent: f32,
    /// Underline centre offset (negative = below the baseline).
    pub underline_offset: f32,
    /// Underline thickness.
    pub underline_thickness: f32,
    /// Strikeout centre offset (positive = above the baseline).
    pub strikeout_offset: f32,
    /// Strikeout thickness.
    pub strikeout_thickness: f32,
}

impl FaceMetrics {
    /// Scale factor from design units to layer pixels at `size_px`.
    #[must_use]
    pub fn scale(&self, size_px: f32) -> f32 {
        if self.units_per_em > 0.0 {
            size_px / self.units_per_em
        } else {
            0.0
        }
    }
}

/// Preference order used to pick a generic family when the platform's own
/// choice is not installed.
const SANS_PREFERENCES: &[&str] = &[
    "Segoe UI",
    "Helvetica Neue",
    "Arial",
    "Noto Sans",
    "DejaVu Sans",
    "Liberation Sans",
];
const SERIF_PREFERENCES: &[&str] = &[
    "Times New Roman",
    "Georgia",
    "Noto Serif",
    "DejaVu Serif",
    "Liberation Serif",
];
const MONO_PREFERENCES: &[&str] = &[
    "Consolas",
    "Menlo",
    "Courier New",
    "Noto Sans Mono",
    "DejaVu Sans Mono",
    "Liberation Mono",
];

/// Environment variable that replaces the system font scan in
/// [`FontLibrary::with_system_fonts`] with a fixed list of directories.
///
/// The value is a `PATH`-style list (`;` on Windows, `:` elsewhere) of
/// directories to scan instead of the machine's font folders. Set but empty
/// means "no system fonts": the library then holds only what callers load
/// through [`FontLibrary::load_bytes`], which is exactly the shape of a bare
/// CI runner and lets that condition be reproduced on any developer machine.
pub const FONT_DIRS_ENV: &str = "RASTER_STUDIO_FONT_DIRS";

/// Owns the font database and the shaping context built on top of it.
#[derive(Debug)]
pub struct FontLibrary {
    system: FontSystem,
    /// The family [`Family::SansSerif`] resolves to after
    /// [`Self::repair_generic_families`] pinned it; empty while the database
    /// is empty. Remembered because the database has no getter, and because
    /// [`Self::substitute_for`] must name exactly what shaping falls back to.
    generic_sans: String,
    /// The family [`Family::Serif`] resolves to; empty while unused.
    generic_serif: String,
    /// The family [`Family::Monospace`] resolves to; empty while unused.
    generic_mono: String,
}

impl FontLibrary {
    /// A library with no fonts at all.
    ///
    /// Useful when the caller wants full control over what is available —
    /// notably the tests, which load one known family so that advances and
    /// line breaks are reproducible on any machine.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            system: FontSystem::new_with_locale_and_db("en-US".to_string(), Database::new()),
            generic_sans: String::new(),
            generic_serif: String::new(),
            generic_mono: String::new(),
        }
    }

    /// A library populated with the fonts installed on this machine.
    ///
    /// Scanning the system font directories takes a noticeable amount of time;
    /// build one library per application and share it.
    ///
    /// When the environment variable [`FONT_DIRS_ENV`] is set, the system scan
    /// is replaced by [`Self::from_font_dirs`] over the directories it lists
    /// (separated the way `PATH` is on this platform; an empty value means no
    /// directories at all). This is the seam that lets a test process stand
    /// in for a machine with a different — or no — font installation, such as
    /// a CI runner that has only a handful of families.
    #[must_use]
    pub fn with_system_fonts() -> Self {
        if let Some(dirs) = std::env::var_os(FONT_DIRS_ENV) {
            return Self::from_font_dirs(std::env::split_paths(&dirs));
        }
        let mut this = Self {
            system: FontSystem::new(),
            generic_sans: String::new(),
            generic_serif: String::new(),
            generic_mono: String::new(),
        };
        this.repair_generic_families();
        this
    }

    /// A library holding only the font files found in `dirs` (scanned
    /// recursively), and nothing from the machine's own installation.
    ///
    /// Directories that do not exist or cannot be read contribute nothing; an
    /// empty iterator yields the same fontless library as [`Self::empty`].
    /// This is what [`Self::with_system_fonts`] builds when [`FONT_DIRS_ENV`]
    /// is set.
    #[must_use]
    pub fn from_font_dirs<I, P>(dirs: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: AsRef<std::path::Path>,
    {
        let mut db = Database::new();
        for dir in dirs {
            let dir = dir.as_ref();
            if dir.as_os_str().is_empty() {
                continue;
            }
            db.load_fonts_dir(dir);
        }
        let mut this = Self {
            system: FontSystem::new_with_locale_and_db("en-US".to_string(), db),
            generic_sans: String::new(),
            generic_serif: String::new(),
            generic_mono: String::new(),
        };
        this.repair_generic_families();
        this
    }

    /// Add every face contained in `data` (a TTF/OTF/TTC/WOFF blob) and return
    /// the handles of the faces that were added.
    pub fn load_bytes(&mut self, data: Vec<u8>) -> Vec<FontId> {
        let source = Source::Binary(Arc::new(data));
        let ids = self.system.db_mut().load_font_source(source);
        let ids: Vec<FontId> = ids.into_iter().map(FontId).collect();
        self.repair_generic_families();
        ids
    }

    /// Number of faces in the library.
    #[must_use]
    pub fn face_count(&self) -> usize {
        self.system.db().len()
    }

    /// Whether the library has no faces at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.system.db().is_empty()
    }

    /// Every family, with its faces — the data behind a family picker.
    #[must_use]
    pub fn families(&self) -> Vec<FamilyRecord> {
        let mut grouped: BTreeMap<String, Vec<FaceRecord>> = BTreeMap::new();
        for info in self.system.db().faces() {
            let record = face_record(info);
            grouped
                .entry(record.family.clone())
                .or_default()
                .push(record);
        }
        grouped
            .into_iter()
            .map(|(name, mut faces)| {
                faces.sort_by_key(|f| (f.weight.0, slant_order(f.slant), f.id));
                FamilyRecord { name, faces }
            })
            .collect()
    }

    /// Just the family names, sorted — the cheap path for the picker.
    #[must_use]
    pub fn family_names(&self) -> Vec<String> {
        let mut names: BTreeSet<String> = BTreeSet::new();
        for info in self.system.db().faces() {
            names.insert(primary_family_name(info));
        }
        names.into_iter().collect()
    }

    /// Whether a family with this exact name is present.
    #[must_use]
    pub fn has_family(&self, name: &str) -> bool {
        self.system
            .db()
            .faces()
            .any(|info| info.families.iter().any(|(f, _)| f == name))
    }

    /// The substitute shaping will use for a family that is not installed.
    ///
    /// Card 022's reporting contract: the answer here and the family
    /// `attrs_for` hands the shaper agree by construction, because both go
    /// through this rule — a missing family shapes with the library's default
    /// sans face ([`Family::SansSerif`], pinned by
    /// [`Self::repair_generic_families`]). `None` means there is nothing to
    /// report: the family is installed, the request is the generic sans
    /// (empty name), or the library has no faces at all so nothing can shape.
    /// The requested name always stays in the document; only shaping sees the
    /// substitute.
    #[must_use]
    pub fn substitute_for(&self, requested: &str) -> Option<String> {
        if requested.is_empty() || self.has_family(requested) || self.is_empty() {
            return None;
        }
        (!self.generic_sans.is_empty()).then(|| self.generic_sans.clone())
    }

    /// Look up one face by handle.
    #[must_use]
    pub fn face(&self, id: FontId) -> Option<FaceRecord> {
        self.system.db().face(id.0).map(face_record)
    }

    /// Match a family/weight/stretch/slant request against the database,
    /// reporting whether the winner has to be faked up to meet the request.
    ///
    /// Like the shaper, a request for a family that is not installed returns
    /// `None` here — check [`Self::substitute_for`] for what shaping would
    /// use instead.
    #[must_use]
    pub fn resolve(
        &self,
        family: &str,
        weight: FontWeight,
        stretch: FontStretch,
        slant: FontSlant,
    ) -> Option<FaceMatch> {
        let named = Family::Name(family);
        let families: &[Family] = if family.is_empty() {
            &[Family::SansSerif]
        } else {
            std::slice::from_ref(&named)
        };
        let query = Query {
            families,
            weight: Weight(weight.0),
            stretch: db_stretch(stretch),
            style: db_style(slant),
        };
        let id = self.system.db().query(&query)?;
        let info = self.system.db().face(id)?;
        Some(FaceMatch {
            id: FontId(id),
            synthetic_bold: weight.needs_synthesis(FontWeight(info.weight.0)),
            synthetic_italic: slant.is_slanted() && info.style == DbStyle::Normal,
        })
    }

    /// Design-unit metrics for a face, instantiated at `weight` (variable
    /// fonts are pinned to that weight axis position).
    pub fn face_metrics(&mut self, id: FontId, weight: FontWeight) -> Option<FaceMetrics> {
        let font = self.system.get_font(id.0, Weight(weight.0))?;
        let m = font.metrics();
        let upem = f32::from(m.units_per_em);
        Some(FaceMetrics {
            units_per_em: upem,
            ascent: m.ascent,
            descent: m.descent,
            underline_offset: m.underline.map_or(-0.1 * upem, |d| d.offset),
            underline_thickness: m.underline.map_or(0.05 * upem, |d| d.thickness),
            strikeout_offset: m.strikeout.map_or(0.26 * upem, |d| d.offset),
            strikeout_thickness: m.strikeout.map_or(0.05 * upem, |d| d.thickness),
        })
    }

    /// Point the generic families (`sans-serif`, `serif`, `monospace`) at
    /// something that actually exists in this database.
    ///
    /// The shaping stack ships hard-coded defaults that are frequently absent,
    /// which would make `Family::SansSerif` resolve to nothing at all.
    fn repair_generic_families(&mut self) {
        let available: BTreeSet<String> = self
            .system
            .db()
            .faces()
            .flat_map(|info| info.families.iter().map(|(f, _)| f.clone()))
            .collect();
        if available.is_empty() {
            self.generic_sans.clear();
            self.generic_serif.clear();
            self.generic_mono.clear();
            return;
        }
        let fallback = available.iter().next().cloned().unwrap_or_else(String::new);
        let mono_fallback = self
            .system
            .db()
            .faces()
            .find(|info| info.monospaced)
            .map_or_else(|| fallback.clone(), primary_family_name);

        let sans = pick(SANS_PREFERENCES, &available).unwrap_or_else(|| fallback.clone());
        let serif = pick(SERIF_PREFERENCES, &available).unwrap_or_else(|| sans.clone());
        let mono = pick(MONO_PREFERENCES, &available).unwrap_or(mono_fallback);

        {
            let db = self.system.db_mut();
            db.set_sans_serif_family(sans.clone());
            db.set_serif_family(serif.clone());
            db.set_monospace_family(mono.clone());
            db.set_cursive_family(sans.clone());
            db.set_fantasy_family(sans.clone());
        }
        self.generic_sans = sans;
        self.generic_serif = serif;
        self.generic_mono = mono;
    }

    /// The shaping stack's font system. Internal: layout and rasterisation
    /// both need mutable access to it.
    pub(crate) fn system_mut(&mut self) -> &mut FontSystem {
        &mut self.system
    }

    /// Weight actually declared by a face, for synthesis decisions taken after
    /// shaping has already chosen a font (including via fallback).
    pub(crate) fn declared_weight(&self, id: FontId) -> Option<FontWeight> {
        self.system
            .db()
            .face(id.0)
            .map(|info| FontWeight(info.weight.0))
    }
}

impl Default for FontLibrary {
    fn default() -> Self {
        Self::empty()
    }
}

fn pick(preferences: &[&str], available: &BTreeSet<String>) -> Option<String> {
    preferences
        .iter()
        .find(|name| available.contains(**name))
        .map(|name| (*name).to_string())
}

fn primary_family_name(info: &cosmic_text::fontdb::FaceInfo) -> String {
    info.families
        .first()
        .map_or_else(|| info.post_script_name.clone(), |(name, _)| name.clone())
}

fn face_record(info: &cosmic_text::fontdb::FaceInfo) -> FaceRecord {
    FaceRecord {
        id: FontId(info.id),
        family: primary_family_name(info),
        post_script_name: info.post_script_name.clone(),
        weight: FontWeight(info.weight.0),
        slant: match info.style {
            DbStyle::Normal => FontSlant::Normal,
            DbStyle::Italic => FontSlant::Italic,
            DbStyle::Oblique => FontSlant::Oblique,
        },
        stretch: FontStretch::from_width_class(info.stretch.to_number()),
        monospaced: info.monospaced,
    }
}

const fn slant_order(slant: FontSlant) -> u8 {
    match slant {
        FontSlant::Normal => 0,
        FontSlant::Italic => 1,
        FontSlant::Oblique => 2,
    }
}

pub(crate) const fn db_style(slant: FontSlant) -> DbStyle {
    match slant {
        FontSlant::Normal => DbStyle::Normal,
        FontSlant::Italic => DbStyle::Italic,
        FontSlant::Oblique => DbStyle::Oblique,
    }
}

pub(crate) const fn db_stretch(stretch: FontStretch) -> Stretch {
    match stretch {
        FontStretch::UltraCondensed => Stretch::UltraCondensed,
        FontStretch::ExtraCondensed => Stretch::ExtraCondensed,
        FontStretch::Condensed => Stretch::Condensed,
        FontStretch::SemiCondensed => Stretch::SemiCondensed,
        FontStretch::Normal => Stretch::Normal,
        FontStretch::SemiExpanded => Stretch::SemiExpanded,
        FontStretch::Expanded => Stretch::Expanded,
        FontStretch::ExtraExpanded => Stretch::ExtraExpanded,
        FontStretch::UltraExpanded => Stretch::UltraExpanded,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh directory under the OS temp folder, unique to this call.
    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        let dir = std::env::temp_dir().join(format!(
            "raster-studio-font-dirs-{tag}-{}-{nanos}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn no_directories_is_a_fontless_library_that_names_no_substitute() {
        let library = FontLibrary::from_font_dirs(std::iter::empty::<&str>());
        assert!(
            library.is_empty(),
            "nothing was scanned, nothing is present"
        );
        assert_eq!(library.substitute_for("Anything"), None);
        // An empty entry (what `PATH`-splitting an empty value can yield) and
        // a directory that does not exist both contribute nothing.
        let missing = scratch_dir("missing").join("does-not-exist");
        let library = FontLibrary::from_font_dirs([std::path::Path::new(""), &missing]);
        assert!(library.is_empty());
    }

    #[test]
    fn a_directory_of_font_files_is_the_whole_library() {
        let dir = scratch_dir("dejavu");
        std::fs::write(dir.join("DejaVuSans.ttf"), dejavu::sans::regular()).unwrap();
        let library = FontLibrary::from_font_dirs([&dir]);
        assert_eq!(library.face_count(), 1, "only the file in the directory");
        assert_eq!(library.family_names(), vec!["DejaVu Sans".to_string()]);
        // The generic sans is pinned to what is there, so a missing family is
        // reported against it — the CI-runner shape this seam exists to model.
        assert_eq!(
            library
                .substitute_for("Raster Test Missing Family")
                .as_deref(),
            Some("DejaVu Sans")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
