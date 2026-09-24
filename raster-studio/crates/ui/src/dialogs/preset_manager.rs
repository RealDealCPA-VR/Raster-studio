//! Edit ▸ Preset Manager (W10-G): every saved preset in one window.
//!
//! One list per kind — brushes, gradients, patterns, styles and custom
//! shapes, the kinds the application's preset store keeps — each renamed,
//! deleted and reordered in place, and the whole library written to or read
//! from a JSON file.
//!
//! The dialog edits a [`PresetLibrary`]: a name and an opaque payload per
//! entry. The payload is the application's (it knows each kind's schema);
//! this module never looks inside it, so a rename here can never corrupt a
//! preset. Confirming hands the edited library back and the shell rebuilds
//! its store from it — one write, so Cancel really does leave every preset as
//! it was. Import and Export ask the host for a file
//! ([`PresetManagerDialog::take_file_request`]); the host answers with
//! [`PresetManagerDialog::import`] or reads [`PresetManagerDialog::library`].

use egui::Context;
use serde::{Deserialize, Serialize};

use super::chrome::{
    action_row_with_extras, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::controls::sidebar_list;
use super::sizes;
use crate::strings::tr;

/// The kinds of preset the library holds.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum PresetKind {
    Brushes,
    Gradients,
    Patterns,
    Styles,
    Shapes,
}

impl PresetKind {
    pub const ALL: [PresetKind; 5] = [
        PresetKind::Brushes,
        PresetKind::Gradients,
        PresetKind::Patterns,
        PresetKind::Styles,
        PresetKind::Shapes,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PresetKind::Brushes => "Brushes",
            PresetKind::Gradients => "Gradients",
            PresetKind::Patterns => "Patterns",
            PresetKind::Styles => "Styles",
            PresetKind::Shapes => "Shapes",
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|k| *k == self).unwrap_or(0)
    }
}

/// One preset: its name, and the application's payload for it.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct PresetEntry {
    pub name: String,
    /// Opaque to this module: the application's serialized preset.
    pub data: String,
}

/// Every preset, per kind, in order.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct PresetLibrary {
    #[serde(default)]
    pub brushes: Vec<PresetEntry>,
    #[serde(default)]
    pub gradients: Vec<PresetEntry>,
    #[serde(default)]
    pub patterns: Vec<PresetEntry>,
    #[serde(default)]
    pub styles: Vec<PresetEntry>,
    #[serde(default)]
    pub shapes: Vec<PresetEntry>,
}

impl PresetLibrary {
    pub fn list(&self, kind: PresetKind) -> &[PresetEntry] {
        match kind {
            PresetKind::Brushes => &self.brushes,
            PresetKind::Gradients => &self.gradients,
            PresetKind::Patterns => &self.patterns,
            PresetKind::Styles => &self.styles,
            PresetKind::Shapes => &self.shapes,
        }
    }

    pub fn list_mut(&mut self, kind: PresetKind) -> &mut Vec<PresetEntry> {
        match kind {
            PresetKind::Brushes => &mut self.brushes,
            PresetKind::Gradients => &mut self.gradients,
            PresetKind::Patterns => &mut self.patterns,
            PresetKind::Styles => &mut self.styles,
            PresetKind::Shapes => &mut self.shapes,
        }
    }

    /// How many presets in all.
    pub fn len(&self) -> usize {
        PresetKind::ALL.iter().map(|k| self.list(*k).len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Rename entry `index`. Refused (`false`) for an empty name, a name the
    /// kind already uses elsewhere, or a missing entry.
    pub fn rename(&mut self, kind: PresetKind, index: usize, name: &str) -> bool {
        let name = name.trim();
        if name.is_empty() {
            return false;
        }
        let list = self.list_mut(kind);
        if index >= list.len()
            || list
                .iter()
                .enumerate()
                .any(|(i, e)| i != index && e.name == name)
        {
            return false;
        }
        list[index].name = name.to_string();
        true
    }

    /// Delete entry `index`, returning it.
    pub fn delete(&mut self, kind: PresetKind, index: usize) -> Option<PresetEntry> {
        let list = self.list_mut(kind);
        (index < list.len()).then(|| list.remove(index))
    }

    /// Move entry `from` to position `to` (clamped). `false` when nothing
    /// moved.
    pub fn reorder(&mut self, kind: PresetKind, from: usize, to: usize) -> bool {
        let list = self.list_mut(kind);
        if from >= list.len() {
            return false;
        }
        let to = to.min(list.len() - 1);
        if from == to {
            return false;
        }
        let entry = list.remove(from);
        list.insert(to, entry);
        true
    }

    /// Merge `other` in: an entry whose name the kind already has replaces
    /// it in place, a new one is appended. Answers how many entries came in.
    pub fn merge(&mut self, mut other: PresetLibrary) -> usize {
        let mut n = 0;
        for kind in PresetKind::ALL {
            let incoming = std::mem::take(other.list_mut(kind));
            let list = self.list_mut(kind);
            for entry in incoming {
                if entry.name.trim().is_empty() {
                    continue;
                }
                match list.iter_mut().find(|e| e.name == entry.name) {
                    Some(slot) => *slot = entry,
                    None => list.push(entry),
                }
                n += 1;
            }
        }
        n
    }
}

/// What the host is asked to do with a file.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PresetFileRequest {
    /// Pick a JSON file and [`PresetManagerDialog::import`] it.
    Import,
    /// Pick a destination and write [`PresetManagerDialog::library`] there.
    Export,
}

/// Edit ▸ Preset Manager.
#[derive(Debug, Clone, PartialEq)]
pub struct PresetManagerDialog {
    library: PresetLibrary,
    kind: usize,
    selected: Option<usize>,
    name: String,
    request: Option<PresetFileRequest>,
    /// The last import/export outcome, shown under the list.
    note: Option<String>,
}

impl PresetManagerDialog {
    pub fn new(library: PresetLibrary) -> Self {
        Self {
            library,
            kind: 0,
            selected: None,
            name: String::new(),
            request: None,
            note: None,
        }
    }

    pub fn title(&self) -> String {
        crate::menu::MenuAction::PresetManager.label()
    }

    pub fn library(&self) -> &PresetLibrary {
        &self.library
    }

    pub fn kind(&self) -> PresetKind {
        PresetKind::ALL[self.kind.min(PresetKind::ALL.len() - 1)]
    }

    pub fn set_kind(&mut self, kind: PresetKind) {
        self.kind = kind.index();
        self.select(None);
    }

    pub fn selected(&self) -> Option<usize> {
        self.selected
    }

    /// Select an entry of the current kind (its name fills the name field).
    pub fn select(&mut self, index: Option<usize>) {
        let kind = self.kind();
        self.selected = index.filter(|i| *i < self.library.list(kind).len());
        self.name = self
            .selected
            .map(|i| self.library.list(kind)[i].name.clone())
            .unwrap_or_default();
    }

    /// Rename the selected entry to `name`.
    pub fn rename_selected(&mut self, name: &str) -> bool {
        let kind = self.kind();
        match self.selected {
            Some(i) if self.library.rename(kind, i, name) => {
                self.name = self.library.list(kind)[i].name.clone();
                true
            }
            _ => false,
        }
    }

    pub fn delete_selected(&mut self) -> bool {
        let kind = self.kind();
        let Some(i) = self.selected else { return false };
        if self.library.delete(kind, i).is_none() {
            return false;
        }
        let len = self.library.list(kind).len();
        self.select((len > 0).then(|| i.min(len - 1)));
        true
    }

    /// Move the selected entry one place up (`-1`) or down (`+1`).
    pub fn move_selected(&mut self, delta: isize) -> bool {
        let kind = self.kind();
        let Some(i) = self.selected else { return false };
        let Some(to) = i.checked_add_signed(delta) else {
            return false;
        };
        if self.library.reorder(kind, i, to) {
            self.selected = Some(to.min(self.library.list(kind).len() - 1));
            true
        } else {
            false
        }
    }

    /// Merge an imported library in; the note says how many came.
    pub fn import(&mut self, other: PresetLibrary) -> usize {
        let n = self.library.merge(other);
        self.note = Some(format!("+{n}"));
        self.select(None);
        n
    }

    /// Show a line under the list (an import/export outcome from the host).
    pub fn set_note(&mut self, note: impl Into<String>) {
        self.note = Some(note.into());
    }

    /// Whether Import or Export was pressed since the last call.
    pub fn take_file_request(&mut self) -> Option<PresetFileRequest> {
        self.request.take()
    }

    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<PresetLibrary> {
        if keys.cancel {
            DialogOutcome::Cancelled
        } else if keys.confirm {
            DialogOutcome::Confirmed(self.library.clone())
        } else {
            DialogOutcome::Open
        }
    }

    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<PresetLibrary> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let title = self.title();
        let drawn = modal(ctx, "preset-manager", &title, None, DialogWidth::Wide, |ui| {
            self.body(ui)
        });
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => DialogOutcome::Confirmed(self.library.clone()),
                DialogButton::Extra(0) => {
                    self.request = Some(PresetFileRequest::Export);
                    DialogOutcome::Open
                }
                DialogButton::Extra(_) => {
                    self.request = Some(PresetFileRequest::Import);
                    DialogOutcome::Open
                }
            };
        }
        outcome
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(sizes::sidebar_width());
                let labels: Vec<&str> = PresetKind::ALL.iter().map(|k| k.label()).collect();
                let mut kind = self.kind;
                if sidebar_list(ui, &mut kind, &labels) {
                    self.kind = kind;
                    self.select(None);
                }
            });
            ui.vertical(|ui| {
                let kind = self.kind();
                let names: Vec<String> = self
                    .library
                    .list(kind)
                    .iter()
                    .map(|e| e.name.clone())
                    .collect();
                egui::ScrollArea::vertical()
                    .id_salt("preset-manager-list")
                    .max_height(sizes::list_max_height())
                    .show(ui, |ui| {
                        for (i, name) in names.iter().enumerate() {
                            if ui
                                .selectable_label(self.selected == Some(i), name)
                                .clicked()
                            {
                                self.select(Some(i));
                            }
                        }
                    });
                if names.is_empty() {
                    caption(ui, tr("ui.adjustment.nothing.to.preview"));
                }
                let has = self.selected.is_some();
                ui.add_enabled_ui(has, |ui| {
                    ui.horizontal(|ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.name)
                                .desired_width(sizes::text_field_name()),
                        );
                        if ui.button("Rename").clicked() {
                            let name = self.name.clone();
                            self.rename_selected(&name);
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.button("Up").clicked() {
                            self.move_selected(-1);
                        }
                        if ui.button("Down").clicked() {
                            self.move_selected(1);
                        }
                        if ui.button("Delete").clicked() {
                            self.delete_selected();
                        }
                    });
                });
                if let Some(note) = &self.note {
                    caption(ui, note.clone());
                }
            });
        });
        action_row_with_extras(
            ui,
            tr("ui.adjustment.confirm"),
            None,
            &[("Export", None), ("Import", None)],
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str) -> PresetEntry {
        PresetEntry {
            name: name.into(),
            data: format!("{{\"n\":\"{name}\"}}"),
        }
    }

    fn library() -> PresetLibrary {
        PresetLibrary {
            brushes: vec![entry("Soft"), entry("Hard"), entry("Chalk")],
            patterns: vec![entry("Dots")],
            ..PresetLibrary::default()
        }
    }

    #[test]
    fn rename_delete_and_reorder_edit_the_library_in_place() {
        let mut d = PresetManagerDialog::new(library());
        d.set_kind(PresetKind::Brushes);
        d.select(Some(1));
        assert!(!d.rename_selected("Soft"), "a duplicate name is refused");
        assert!(!d.rename_selected("  "), "an empty name is refused");
        assert!(d.rename_selected("Hard Round"));
        assert!(d.move_selected(-1));
        assert_eq!(d.selected(), Some(0));
        let names: Vec<_> = d.library().brushes.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Hard Round", "Soft", "Chalk"]);
        // The payload travels with the entry, untouched.
        assert_eq!(d.library().brushes[0].data, "{\"n\":\"Hard\"}");
        d.select(Some(2));
        assert!(d.delete_selected());
        assert_eq!(d.library().brushes.len(), 2);
        assert_eq!(d.selected(), Some(1));
        assert!(!d.move_selected(1), "the last cannot move down");
        match d.resolve(DialogKeys::CONFIRM) {
            DialogOutcome::Confirmed(lib) => assert_eq!(&lib, d.library()),
            other => panic!("{other:?}"),
        }
        assert_eq!(d.resolve(DialogKeys::CANCEL), DialogOutcome::Cancelled);
    }

    #[test]
    fn import_merges_by_name_and_round_trips_through_json() {
        let mut d = PresetManagerDialog::new(library());
        let json = serde_json::to_string(d.library()).unwrap();
        let back: PresetLibrary = serde_json::from_str(&json).unwrap();
        assert_eq!(&back, d.library());
        let incoming = PresetLibrary {
            brushes: vec![PresetEntry {
                name: "Soft".into(),
                data: "new".into(),
            }],
            styles: vec![entry("Glow")],
            ..PresetLibrary::default()
        };
        assert_eq!(d.import(incoming), 2);
        assert_eq!(d.library().brushes.len(), 3, "same name replaces");
        assert_eq!(d.library().brushes[0].data, "new");
        assert_eq!(d.library().styles.len(), 1);
    }

    #[test]
    fn it_draws_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut d = PresetManagerDialog::new(library());
            d.select(Some(0));
            assert!(d.show(ctx).is_open());
        });
    }
}
