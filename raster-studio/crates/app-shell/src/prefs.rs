//! Where the application keeps its own state, and what it keeps there.
//!
//! Three files, all JSON, all in one directory:
//!
//! ```text
//! preferences.json   theme, UI scale, units, language, wheel, autosave,
//!                    history depth, scratch, keymap
//! recent.json        the recent-files list (see [`crate::recent`])
//! sessions/{pid}.json  one "this run is alive" marker per running instance
//!                      (see [`crate::session`])
//! ```
//!
//! Everything here loads **infallibly**. A preferences file that is missing,
//! truncated, or written by a newer build must not stop the application from
//! starting, so [`Preferences::load`] falls back to the defaults and every
//! field is clamped on the way in ([`Preferences::sanitized`]) rather than
//! trusted. A UI scale of `0.0` read from disk would otherwise divide the whole
//! layout by zero.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::keymap::KeyOverride;

/// Directory layout for the application's own files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    root: PathBuf,
}

impl AppPaths {
    /// The per-user configuration directory, or a temporary one if the platform
    /// will not name it. Never fails: a machine with no config directory still
    /// gets a working editor, it just forgets its preferences.
    pub fn discover() -> AppPaths {
        let root = dirs::config_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("RasterStudio");
        AppPaths { root }
    }

    /// A layout rooted anywhere — what the tests use.
    pub fn rooted(root: impl Into<PathBuf>) -> AppPaths {
        AppPaths { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn preferences_file(&self) -> PathBuf {
        self.root.join("preferences.json")
    }

    /// The user preset store's file (named patterns and brushes).
    pub fn presets_file(&self) -> PathBuf {
        self.root.join("presets.json")
    }

    pub fn recent_file(&self) -> PathBuf {
        self.root.join("recent.json")
    }

    /// Directory holding one session marker per *running* instance.
    ///
    /// One file per process id rather than one file per installation: two
    /// copies of the editor running at once used to share a single
    /// `session.json`, so the second overwrote the first's record and a later
    /// crash of the first recovered nothing. See [`crate::session`].
    pub fn session_dir(&self) -> PathBuf {
        self.root.join("sessions")
    }

    /// This run's marker file.
    pub fn session_file_for(&self, pid: u32) -> PathBuf {
        self.session_dir().join(format!("{pid}.json"))
    }

    /// The single marker file builds before the per-process layout wrote. Still
    /// read at start-up so a crash of the older build is still recoverable, and
    /// removed once it has been.
    pub fn legacy_session_file(&self) -> PathBuf {
        self.root.join("session.json")
    }

    /// Where autosaves of never-saved documents go.
    pub fn default_scratch_dir(&self) -> PathBuf {
        self.root.join("scratch")
    }

    /// Create the directory if it is not there yet.
    pub fn ensure(&self) -> io::Result<()> {
        std::fs::create_dir_all(&self.root)
    }
}

/// The user's theme choice.
///
/// Photopea's dark grey is the shipped default: a fresh profile launches dark
/// on any host. `System` is the opt-in "follow the OS" mode, and stays a real
/// third state rather than a synonym for one of the other two — it has to
/// track the OS while the app is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    Light,
    #[default]
    Dark,
    /// Opt-in: follow the operating system's light/dark setting.
    System,
}

impl ThemeChoice {
    pub const ALL: &'static [ThemeChoice] =
        &[ThemeChoice::Light, ThemeChoice::Dark, ThemeChoice::System];

    /// The theme to install, given what the OS currently reports.
    pub fn resolve(self, system: design::Theme) -> design::Theme {
        match self {
            ThemeChoice::Light => design::Theme::Light,
            ThemeChoice::Dark => design::Theme::Dark,
            ThemeChoice::System => system,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            ThemeChoice::Light => "Light",
            ThemeChoice::Dark => "Dark",
            ThemeChoice::System => "System",
        }
    }
}

impl From<ThemeChoice> for ui::dialogs::ThemeChoice {
    fn from(t: ThemeChoice) -> Self {
        match t {
            ThemeChoice::Light => Self::Light,
            ThemeChoice::Dark => Self::Dark,
            ThemeChoice::System => Self::System,
        }
    }
}

impl From<ui::dialogs::ThemeChoice> for ThemeChoice {
    fn from(t: ui::dialogs::ThemeChoice) -> Self {
        match t {
            ui::dialogs::ThemeChoice::Light => Self::Light,
            ui::dialogs::ThemeChoice::Dark => Self::Dark,
            ui::dialogs::ThemeChoice::System => Self::System,
        }
    }
}

/// The application-wide measurement unit: what the rulers read in and what
/// the size readouts the editor writes are spelled in.
///
/// Its own serialized enum rather than [`ui::dialogs::Unit`] so the file
/// format does not move when the dialog crate renames a variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UnitChoice {
    #[default]
    Px,
    In,
    Cm,
    Mm,
    Pt,
    /// Picas, six to the inch. Added after the file format shipped: an older
    /// file never names it, and every name an older file can hold still reads
    /// as before, so the addition is backward compatible by construction.
    Pc,
    Percent,
}

impl From<UnitChoice> for ui::dialogs::Unit {
    fn from(u: UnitChoice) -> Self {
        match u {
            UnitChoice::Px => Self::Pixels,
            UnitChoice::In => Self::Inches,
            UnitChoice::Cm => Self::Centimeters,
            UnitChoice::Mm => Self::Millimeters,
            UnitChoice::Pt => Self::Points,
            UnitChoice::Pc => Self::Picas,
            UnitChoice::Percent => Self::Percent,
        }
    }
}

impl From<ui::dialogs::Unit> for UnitChoice {
    /// Lossless: every unit View ▸ Rulers offers has its own choice, so the
    /// unit a ruler menu pick persists is the unit the next launch restores.
    fn from(u: ui::dialogs::Unit) -> Self {
        match u {
            ui::dialogs::Unit::Pixels => Self::Px,
            ui::dialogs::Unit::Inches => Self::In,
            ui::dialogs::Unit::Centimeters => Self::Cm,
            ui::dialogs::Unit::Millimeters => Self::Mm,
            ui::dialogs::Unit::Points => Self::Pt,
            ui::dialogs::Unit::Picas => Self::Pc,
            ui::dialogs::Unit::Percent => Self::Percent,
        }
    }
}

/// Window size and position, restored between sessions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowGeometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub maximized: bool,
}

impl WindowGeometry {
    pub const MIN_WIDTH: u32 = 720;
    pub const MIN_HEIGHT: u32 = 480;
    /// Refuse a restored size larger than this. A corrupt or stale record can
    /// otherwise ask for a window no display can show.
    pub const MAX_EDGE: u32 = 16_384;

    pub const DEFAULT: WindowGeometry = WindowGeometry {
        x: 64,
        y: 64,
        width: 1440,
        height: 900,
        maximized: false,
    };

    /// Clamp a record read from disk into something a window manager can honour.
    pub fn sanitized(self) -> WindowGeometry {
        WindowGeometry {
            x: self
                .x
                .clamp(-(Self::MAX_EDGE as i32), Self::MAX_EDGE as i32),
            y: self
                .y
                .clamp(-(Self::MAX_EDGE as i32), Self::MAX_EDGE as i32),
            width: self.width.clamp(Self::MIN_WIDTH, Self::MAX_EDGE),
            height: self.height.clamp(Self::MIN_HEIGHT, Self::MAX_EDGE),
            maximized: self.maximized,
        }
    }
}

impl Default for WindowGeometry {
    fn default() -> Self {
        Self::DEFAULT
    }
}

fn default_ui_scale() -> f32 {
    1.0
}

fn default_autosave() -> u64 {
    300
}

fn default_history_depth() -> usize {
    editor_core::DEFAULT_HISTORY_LIMIT
}

fn default_true() -> bool {
    true
}

fn default_language() -> String {
    ui::strings::Locale::En.code().to_string()
}

/// Everything the application remembers about how the user likes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub theme: ThemeChoice,
    /// Multiplier on egui's points-per-pixel.
    #[serde(default = "default_ui_scale")]
    pub ui_scale: f32,
    /// Seconds between autosaves. `0` disables autosave entirely.
    #[serde(default = "default_autosave")]
    pub autosave_interval_secs: u64,
    #[serde(default = "default_history_depth")]
    pub history_depth: usize,
    /// Where autosaves and other working files go. `None` means
    /// [`AppPaths::default_scratch_dir`].
    pub scratch_dir: Option<PathBuf>,
    pub keymap_overrides: Vec<KeyOverride>,
    pub window: Option<WindowGeometry>,
    /// The measurement unit the rulers and the size readouts use.
    pub units: UnitChoice,
    /// The strings-catalogue locale, as its BCP-47 code. A code the catalogue
    /// does not carry is replaced by English on load.
    #[serde(default = "default_language")]
    pub language: String,
    /// A plain wheel zooms the canvas (Photopea's default). Off, the plain
    /// wheel pans and Ctrl+wheel zooms.
    #[serde(default = "default_true")]
    pub scroll_wheel_zooms: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Preferences {
            theme: ThemeChoice::default(),
            ui_scale: default_ui_scale(),
            autosave_interval_secs: default_autosave(),
            history_depth: default_history_depth(),
            scratch_dir: None,
            keymap_overrides: Vec::new(),
            window: None,
            units: UnitChoice::default(),
            language: default_language(),
            scroll_wheel_zooms: true,
        }
    }
}

impl Preferences {
    pub const MIN_UI_SCALE: f32 = 0.5;
    pub const MAX_UI_SCALE: f32 = 3.0;
    /// Shortest autosave period that is not "off". Anything between 1 and this
    /// is raised rather than honoured: a one-second autosave of a large
    /// document would keep the disk permanently busy.
    pub const MIN_AUTOSAVE_SECS: u64 = 15;
    pub const MAX_AUTOSAVE_SECS: u64 = 60 * 60;
    pub const MIN_HISTORY_DEPTH: usize = 1;
    pub const MAX_HISTORY_DEPTH: usize = 10_000;

    /// Clamp every field into its supported range.
    ///
    /// Applied on load *and* on save, so a value that reaches the file by any
    /// route is one the running application would also accept.
    pub fn sanitized(mut self) -> Self {
        self.ui_scale = if self.ui_scale.is_finite() {
            self.ui_scale.clamp(Self::MIN_UI_SCALE, Self::MAX_UI_SCALE)
        } else {
            default_ui_scale()
        };
        if self.autosave_interval_secs != 0 {
            self.autosave_interval_secs = self
                .autosave_interval_secs
                .clamp(Self::MIN_AUTOSAVE_SECS, Self::MAX_AUTOSAVE_SECS);
        }
        self.history_depth = self
            .history_depth
            .clamp(Self::MIN_HISTORY_DEPTH, Self::MAX_HISTORY_DEPTH);
        self.window = self.window.map(WindowGeometry::sanitized);
        self.language = self.locale().code().to_string();
        // An empty scratch path is "the default", not "the working directory".
        if self
            .scratch_dir
            .as_ref()
            .is_some_and(|p| p.as_os_str().to_string_lossy().trim().is_empty())
        {
            self.scratch_dir = None;
        }
        self
    }

    /// The catalogue locale the language code names (English for any code the
    /// catalogue does not carry).
    pub fn locale(&self) -> ui::strings::Locale {
        ui::strings::Locale::from_code(&self.language)
    }

    pub fn autosave_interval(&self) -> Option<std::time::Duration> {
        (self.autosave_interval_secs > 0)
            .then(|| std::time::Duration::from_secs(self.autosave_interval_secs))
    }

    /// The scratch directory in force, given where the app keeps its files.
    pub fn scratch_dir(&self, paths: &AppPaths) -> PathBuf {
        self.scratch_dir
            .clone()
            .unwrap_or_else(|| paths.default_scratch_dir())
    }

    /// Read preferences, falling back to the defaults for anything unreadable.
    pub fn load(path: &Path) -> Preferences {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(_) => return Preferences::default(),
        };
        match serde_json::from_str::<Preferences>(&text) {
            Ok(p) => p.sanitized(),
            Err(e) => {
                tracing::warn!("preferences at {} are unreadable: {e}", path.display());
                Preferences::default()
            }
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&self.clone().sanitized())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn preferences_round_trip_through_disk() {
        let dir = tmp();
        let paths = AppPaths::rooted(dir.path());
        let prefs = Preferences {
            theme: ThemeChoice::Light,
            ui_scale: 1.25,
            autosave_interval_secs: 60,
            history_depth: 42,
            scratch_dir: Some(dir.path().join("scratch-elsewhere")),
            keymap_overrides: vec![KeyOverride {
                chord: "Ctrl+K".parse().unwrap(),
                action: Some(crate::Action::Export),
            }],
            window: Some(WindowGeometry {
                x: 10,
                y: 20,
                width: 1000,
                height: 800,
                maximized: false,
            }),
            units: UnitChoice::Cm,
            language: "en".to_string(),
            scroll_wheel_zooms: false,
        };

        prefs.save(&paths.preferences_file()).unwrap();
        let back = Preferences::load(&paths.preferences_file());
        assert_eq!(back, prefs);
    }

    #[test]
    fn a_missing_or_corrupt_file_reads_as_the_defaults() {
        let dir = tmp();
        let path = dir.path().join("preferences.json");
        assert_eq!(Preferences::load(&path), Preferences::default());
        std::fs::write(&path, "{ this is not json").unwrap();
        assert_eq!(Preferences::load(&path), Preferences::default());
        // A partial file keeps what it has and defaults the rest.
        std::fs::write(&path, r#"{"theme":"dark"}"#).unwrap();
        let p = Preferences::load(&path);
        assert_eq!(p.theme, ThemeChoice::Dark);
        assert_eq!(p.ui_scale, 1.0);
    }

    #[test]
    fn every_field_is_clamped_on_the_way_in() {
        let dir = tmp();
        let path = dir.path().join("preferences.json");
        std::fs::write(
            &path,
            r#"{"ui_scale":0.0,"autosave_interval_secs":1,"history_depth":0,
                "window":{"x":0,"y":0,"width":1,"height":1,"maximized":false}}"#,
        )
        .unwrap();
        let p = Preferences::load(&path);
        assert!(p.ui_scale >= Preferences::MIN_UI_SCALE, "{}", p.ui_scale);
        assert_eq!(p.autosave_interval_secs, Preferences::MIN_AUTOSAVE_SECS);
        assert_eq!(p.history_depth, Preferences::MIN_HISTORY_DEPTH);
        let w = p.window.unwrap();
        assert_eq!(w.width, WindowGeometry::MIN_WIDTH);
        assert_eq!(w.height, WindowGeometry::MIN_HEIGHT);

        // A non-finite scale is replaced, not clamped to an edge.
        std::fs::write(&path, r#"{"ui_scale":null}"#).unwrap();
        assert_eq!(Preferences::load(&path).ui_scale, 1.0);
    }

    #[test]
    fn zero_means_autosave_is_off() {
        let mut p = Preferences::default();
        assert!(p.autosave_interval().is_some());
        p.autosave_interval_secs = 0;
        let p = p.sanitized();
        assert_eq!(p.autosave_interval_secs, 0, "0 must not be clamped up");
        assert_eq!(p.autosave_interval(), None);
    }

    #[test]
    fn the_theme_choice_resolves_against_the_system() {
        assert_eq!(
            ThemeChoice::System.resolve(design::Theme::Light),
            design::Theme::Light
        );
        assert_eq!(
            ThemeChoice::System.resolve(design::Theme::Dark),
            design::Theme::Dark
        );
        assert_eq!(
            ThemeChoice::Dark.resolve(design::Theme::Light),
            design::Theme::Dark
        );
        assert_eq!(
            ThemeChoice::Light.resolve(design::Theme::Dark),
            design::Theme::Light
        );
    }

    #[test]
    fn an_older_file_reads_the_new_fields_as_their_defaults() {
        // A preferences file written before units / language / wheel existed.
        let dir = tmp();
        let path = dir.path().join("preferences.json");
        std::fs::write(&path, r#"{"theme":"light","ui_scale":1.5}"#).unwrap();
        let p = Preferences::load(&path);
        assert_eq!(p.units, UnitChoice::Px);
        assert_eq!(p.language, "en");
        assert!(p.scroll_wheel_zooms, "a plain wheel zooms by default");
    }

    #[test]
    fn an_unknown_language_code_falls_back_to_english_on_load() {
        let dir = tmp();
        let path = dir.path().join("preferences.json");
        std::fs::write(&path, r#"{"language":"xx-not-a-locale","units":"cm"}"#).unwrap();
        let p = Preferences::load(&path);
        assert_eq!(p.language, "en");
        assert_eq!(p.locale(), ui::strings::Locale::En);
        assert_eq!(p.units, UnitChoice::Cm);
    }

    #[test]
    fn every_unit_choice_round_trips_through_the_dialogs_unit() {
        for u in [
            UnitChoice::Px,
            UnitChoice::In,
            UnitChoice::Cm,
            UnitChoice::Mm,
            UnitChoice::Pt,
            UnitChoice::Pc,
            UnitChoice::Percent,
        ] {
            let dialog: ui::dialogs::Unit = u.into();
            assert!(ui::dialogs::Unit::PREFERENCE_CHOICES.contains(&dialog));
            assert_eq!(UnitChoice::from(dialog), u);
        }
    }

    /// W3-X: View ▸ Rulers lists every unit, so every one of them must
    /// survive the trip into the preference and back — Picas used to land as
    /// pixels.
    #[test]
    fn every_unit_the_rulers_offer_survives_the_preference_round_trip() {
        for unit in ui::dialogs::Unit::ALL {
            let back: ui::dialogs::Unit = UnitChoice::from(*unit).into();
            assert_eq!(back, *unit);
        }
    }

    #[test]
    fn picas_are_saved_by_name_and_restored_by_a_fresh_load() {
        let dir = tmp();
        let path = dir.path().join("preferences.json");
        let prefs = Preferences {
            units: UnitChoice::from(ui::dialogs::Unit::Picas),
            ..Preferences::default()
        };
        prefs.save(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(r#""units": "pc""#), "{text}");
        let loaded = Preferences::load(&path);
        assert_eq!(loaded.units, UnitChoice::Pc);
        assert_eq!(
            ui::dialogs::Unit::from(loaded.units),
            ui::dialogs::Unit::Picas
        );
    }

    #[test]
    fn a_file_written_before_picas_existed_still_loads_every_unit_it_could_name() {
        let dir = tmp();
        let path = dir.path().join("preferences.json");
        for (name, unit) in [
            ("px", UnitChoice::Px),
            ("in", UnitChoice::In),
            ("cm", UnitChoice::Cm),
            ("mm", UnitChoice::Mm),
            ("pt", UnitChoice::Pt),
            ("percent", UnitChoice::Percent),
        ] {
            std::fs::write(
                &path,
                format!(r#"{{"theme":"dark","ui_scale":1.25,"units":"{name}"}}"#),
            )
            .unwrap();
            let p = Preferences::load(&path);
            assert_eq!(p.units, unit, "{name}");
            assert_eq!(
                p.theme,
                ThemeChoice::Dark,
                "{name}: the rest of the file was kept"
            );
        }
    }

    #[test]
    fn the_scratch_directory_defaults_under_the_app_directory() {
        let dir = tmp();
        let paths = AppPaths::rooted(dir.path());
        let mut prefs = Preferences::default();
        assert_eq!(prefs.scratch_dir(&paths), paths.default_scratch_dir());
        prefs.scratch_dir = Some(PathBuf::from("/elsewhere"));
        assert_eq!(prefs.scratch_dir(&paths), PathBuf::from("/elsewhere"));
    }
}
