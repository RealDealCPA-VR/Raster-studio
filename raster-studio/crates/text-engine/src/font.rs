//! Font discovery, loading and matching.
//!
//! [`FontLibrary`] owns the shaping stack's font database. It can be built
//! empty (deterministic — used by the tests), from the fonts installed on the
//! machine, or from raw bytes handed in by the document (embedded fonts).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use cosmic_text::fontdb::{Database, Family, Query, Source, Stretch, Style as DbStyle, Weight};
use cosmic_text::FontSystem;

use crate::style::{FontSlant, FontWeight};

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

/// Owns the font database and the shaping context built on top of it.
#[derive(Debug)]
pub struct FontLibrary {
    system: FontSystem,
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
        }
    }

    /// A library populated with the fonts installed on this machine.
    ///
    /// Scanning the system font directories takes a noticeable amount of time;
    /// build one library per application and share it.
    #[must_use]
    pub fn with_system_fonts() -> Self {
        let mut this = Self {
            system: FontSystem::new(),
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

    /// Look up one face by handle.
    #[must_use]
    pub fn face(&self, id: FontId) -> Option<FaceRecord> {
        self.system.db().face(id.0).map(face_record)
    }

    /// Match a family/weight/slant request against the database, reporting
    /// whether the winner has to be faked up to meet the request.
    #[must_use]
    pub fn resolve(&self, family: &str, weight: FontWeight, slant: FontSlant) -> Option<FaceMatch> {
        let named = Family::Name(family);
        let families: &[Family] = if family.is_empty() {
            &[Family::SansSerif]
        } else {
            std::slice::from_ref(&named)
        };
        let query = Query {
            families,
            weight: Weight(weight.0),
            stretch: Stretch::Normal,
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

        let db = self.system.db_mut();
        db.set_sans_serif_family(sans.clone());
        db.set_serif_family(serif);
        db.set_monospace_family(mono);
        db.set_cursive_family(sans.clone());
        db.set_fantasy_family(sans);
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
