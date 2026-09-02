//! Where the application keeps its own state, and what it keeps there.
//!
//! Three files, all JSON, all in one directory:
//!
//! ```text
//! preferences.json   theme, UI scale, autosave, history depth, scratch, keymap
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
        self
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
    fn the_scratch_directory_defaults_under_the_app_directory() {
        let dir = tmp();
        let paths = AppPaths::rooted(dir.path());
        let mut prefs = Preferences::default();
        assert_eq!(prefs.scratch_dir(&paths), paths.default_scratch_dir());
        prefs.scratch_dir = Some(PathBuf::from("/elsewhere"));
        assert_eq!(prefs.scratch_dir(&paths), PathBuf::from("/elsewhere"));
    }
}
