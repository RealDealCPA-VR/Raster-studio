//! W10-E: Image ▸ Variables ▸ Define… and Image ▸ Variables ▸ Data Sets….
//!
//! Photopea's (and Photoshop's) data-driven graphics: a **variable** is a
//! name bound to a layer — a *text replacement* variable to a text layer,
//! whose text a data set replaces, or a *visibility* variable to any layer,
//! which a data set shows or hides. A **data set** is one value per
//! variable. Data sets are imported from a CSV whose first row names the
//! variables and whose every further row is one set; the dialog previews one
//! set on the document or exports one file per set.
//!
//! This module owns the question and the pure parts of the answer — the
//! definitions, the CSV reader ([`parse_csv`], [`data_sets_from_csv`]) and
//! the dialog. Applying a set to a document and writing the files is the
//! application's (`app_shell::variables`).

use egui::Context;
use layer_model::LayerId;

use super::batch::BatchFormat;
use super::chrome::{
    action_row, caption, modal, warning, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::controls::{checkbox_row, combo};
use super::sizes;
use crate::strings::tr;

/// What a variable does to the layer it is bound to.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub enum VariableKind {
    /// Replace the text layer's text.
    Text,
    /// Show or hide the layer.
    Visibility,
}

/// One variable: a name bound to a layer.
#[derive(Clone, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct VariableDef {
    pub name: String,
    pub kind: VariableKind,
    pub layer: LayerId,
}

/// One data set: its name and a value per variable, in any order.
#[derive(Clone, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct DataSet {
    pub name: String,
    pub values: Vec<(String, String)>,
}

impl DataSet {
    /// The value this set gives `variable`, if it names it.
    pub fn value(&self, variable: &str) -> Option<&str> {
        self.values
            .iter()
            .find(|(k, _)| k == variable)
            .map(|(_, v)| v.as_str())
    }
}

/// How a visibility variable reads a data-set value: `true`, `1`, `yes`,
/// `visible` and `show` (any case) show the layer; anything else hides it.
pub fn visibility_value(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "true" | "1" | "yes" | "visible" | "show"
    )
}

/// Read CSV text (RFC 4180: comma separated, `"` quoted, `""` an escaped
/// quote, quoted fields may span lines; CRLF or LF) into rows of fields.
/// Blank lines are skipped.
pub fn parse_csv(text: &str) -> Result<Vec<Vec<String>>, String> {
    let mut rows = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut at_field_start = true;
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if at_field_start => {
                quoted = true;
                at_field_start = false;
            }
            ',' => {
                row.push(std::mem::take(&mut field));
                at_field_start = true;
            }
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                if !(row.len() == 1 && row[0].trim().is_empty()) {
                    rows.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
                at_field_start = true;
            }
            c => {
                field.push(c);
                at_field_start = false;
            }
        }
    }
    if quoted {
        return Err(tr("ui.variables.csv.unclosed").to_string());
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        if !(row.len() == 1 && row[0].trim().is_empty()) {
            rows.push(row);
        }
    }
    Ok(rows)
}

/// The data sets a CSV describes, checked against the defined variables:
/// the header row names variables (every one must be defined), each further
/// row is one set named "Data Set N". A row with the wrong number of fields
/// is refused, never padded.
pub fn data_sets_from_csv(text: &str, defs: &[VariableDef]) -> Result<Vec<DataSet>, String> {
    let rows = parse_csv(text)?;
    let Some((header, body)) = rows.split_first() else {
        return Err(tr("ui.variables.csv.empty").to_string());
    };
    let header: Vec<String> = header.iter().map(|h| h.trim().to_string()).collect();
    for name in &header {
        if !defs.iter().any(|d| &d.name == name) {
            return Err(format!("{} {name}", tr("ui.variables.csv.unknown")));
        }
    }
    if body.is_empty() {
        return Err(tr("ui.variables.csv.no.rows").to_string());
    }
    let mut sets = Vec::with_capacity(body.len());
    for (i, row) in body.iter().enumerate() {
        if row.len() != header.len() {
            return Err(format!(
                "{} {} ({}/{})",
                tr("ui.variables.csv.row.width"),
                i + 2,
                row.len(),
                header.len()
            ));
        }
        sets.push(DataSet {
            name: format!("{} {}", tr("ui.variables.set"), i + 1),
            values: header.iter().cloned().zip(row.iter().cloned()).collect(),
        });
    }
    Ok(sets)
}

/// A layer the Define page can bind a variable to.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VariableLayer {
    pub layer: LayerId,
    pub name: String,
    /// Whether a text replacement variable can bind to it.
    pub is_text: bool,
}

/// Which page the dialog shows.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum VariablesPage {
    Define,
    DataSets,
}

/// What the confirmation asks the application to do besides storing the
/// definitions and sets.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VariablesRequest {
    /// Keep the definitions and data sets with the document.
    Save,
    /// Apply this data set to the document, as one undoable step.
    Preview(usize),
    /// Write one file per data set into a folder the host asks for.
    Export(BatchFormat),
}

/// A confirmed Variables dialog.
#[derive(Clone, PartialEq, Debug)]
pub struct VariablesSpec {
    pub defs: Vec<VariableDef>,
    pub sets: Vec<DataSet>,
    pub request: VariablesRequest,
}

/// One Define-page row's edit state.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Binding {
    text_on: bool,
    text_name: String,
    visibility_on: bool,
    visibility_name: String,
}

/// Image ▸ Variables ▸ Define… / Data Sets….
#[derive(Clone, Debug)]
pub struct VariablesDialog {
    page: VariablesPage,
    layers: Vec<VariableLayer>,
    bindings: Vec<Binding>,
    sets: Vec<DataSet>,
    selected_set: usize,
    format: BatchFormat,
    csv_request: bool,
    csv_error: Option<String>,
    request: VariablesRequest,
}

impl VariablesDialog {
    /// Over `layers` (top first), with the document's current `defs` and
    /// `sets`, opened on `page`.
    pub fn new(
        page: VariablesPage,
        layers: Vec<VariableLayer>,
        defs: &[VariableDef],
        sets: Vec<DataSet>,
    ) -> Self {
        let bindings = layers
            .iter()
            .map(|l| {
                let text = defs
                    .iter()
                    .find(|d| d.layer == l.layer && d.kind == VariableKind::Text);
                let vis = defs
                    .iter()
                    .find(|d| d.layer == l.layer && d.kind == VariableKind::Visibility);
                Binding {
                    text_on: text.is_some() && l.is_text,
                    text_name: text.map_or_else(|| default_name(&l.name), |d| d.name.clone()),
                    visibility_on: vis.is_some(),
                    visibility_name: vis.map_or_else(
                        || format!("{}_visible", default_name(&l.name)),
                        |d| d.name.clone(),
                    ),
                }
            })
            .collect();
        Self {
            page,
            layers,
            bindings,
            sets,
            selected_set: 0,
            format: BatchFormat::Png,
            csv_request: false,
            csv_error: None,
            request: VariablesRequest::Save,
        }
    }

    pub fn page(&self) -> VariablesPage {
        self.page
    }

    pub fn set_page(&mut self, page: VariablesPage) {
        self.page = page;
    }

    /// Bind (or unbind, with `None`) a variable of `kind` named `name` to the
    /// layer at `index` in the Define list — what ticking a row does.
    pub fn bind(&mut self, index: usize, kind: VariableKind, name: Option<&str>) {
        let Some(b) = self.bindings.get_mut(index) else {
            return;
        };
        let text_ok = self.layers.get(index).is_some_and(|l| l.is_text);
        match kind {
            VariableKind::Text => {
                b.text_on = name.is_some() && text_ok;
                if let Some(n) = name {
                    b.text_name = n.to_string();
                }
            }
            VariableKind::Visibility => {
                b.visibility_on = name.is_some();
                if let Some(n) = name {
                    b.visibility_name = n.to_string();
                }
            }
        }
    }

    /// The definitions the Define page describes.
    pub fn defs(&self) -> Vec<VariableDef> {
        let mut out = Vec::new();
        for (layer, b) in self.layers.iter().zip(&self.bindings) {
            if b.text_on && layer.is_text {
                out.push(VariableDef {
                    name: b.text_name.trim().to_string(),
                    kind: VariableKind::Text,
                    layer: layer.layer,
                });
            }
            if b.visibility_on {
                out.push(VariableDef {
                    name: b.visibility_name.trim().to_string(),
                    kind: VariableKind::Visibility,
                    layer: layer.layer,
                });
            }
        }
        out
    }

    /// The data sets as they stand.
    pub fn sets(&self) -> &[DataSet] {
        &self.sets
    }

    /// Choose the data set Preview applies.
    pub fn select_set(&mut self, index: usize) {
        self.selected_set = index.min(self.sets.len().saturating_sub(1));
    }

    /// The format Export writes.
    pub fn set_format(&mut self, format: BatchFormat) {
        self.format = format;
    }

    /// Whether the Import CSV button was pressed since the last take — the
    /// host answers it by reading a file and calling [`Self::load_csv`].
    pub fn take_csv_request(&mut self) -> bool {
        std::mem::take(&mut self.csv_request)
    }

    /// Replace the data sets with a CSV's. On failure the sets are kept and
    /// the reason is shown.
    pub fn load_csv(&mut self, text: &str) -> Result<usize, String> {
        match data_sets_from_csv(text, &self.defs()) {
            Ok(sets) => {
                let n = sets.len();
                self.sets = sets;
                self.selected_set = 0;
                self.csv_error = None;
                Ok(n)
            }
            Err(e) => {
                self.csv_error = Some(e.clone());
                Err(e)
            }
        }
    }

    /// Say why a CSV could not be read (the host's file read failed).
    pub fn set_csv_error(&mut self, error: impl Into<String>) {
        self.csv_error = Some(error.into());
    }

    /// Ask for `request` on the next confirmation (tests; the buttons set it).
    pub fn set_request(&mut self, request: VariablesRequest) {
        self.request = request;
    }

    pub fn title(&self) -> &'static str {
        match self.page {
            VariablesPage::Define => tr("ui.variables.define.title"),
            VariablesPage::DataSets => tr("ui.variables.sets.title"),
        }
    }

    /// Why a confirmation is impossible, or `None`.
    pub fn blocked_reason(&self) -> Option<String> {
        let defs = self.defs();
        let mut names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        if names.iter().any(|n| n.is_empty()) {
            return Some(tr("ui.variables.name.empty").to_string());
        }
        names.sort_unstable();
        if names.windows(2).any(|w| w[0] == w[1]) {
            return Some(tr("ui.variables.name.twice").to_string());
        }
        match self.request {
            VariablesRequest::Preview(_) | VariablesRequest::Export(_) if self.sets.is_empty() => {
                Some(tr("ui.variables.no.sets").to_string())
            }
            _ => None,
        }
    }

    /// The spec a confirmation hands over, or `None` while blocked.
    pub fn confirm(&self) -> Option<VariablesSpec> {
        self.blocked_reason().is_none().then(|| VariablesSpec {
            defs: self.defs(),
            sets: self.sets.clone(),
            request: self.request,
        })
    }

    /// Escape and Enter, without drawing — Escape wins over Enter.
    pub fn resolve(&self, keys: DialogKeys) -> DialogOutcome<VariablesSpec> {
        if keys.cancel {
            return DialogOutcome::Cancelled;
        }
        if keys.confirm {
            if let Some(spec) = self.confirm() {
                return DialogOutcome::Confirmed(spec);
            }
        }
        DialogOutcome::Open
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<VariablesSpec> {
        let mut outcome = self.resolve(DialogKeys::read(ctx));
        let title = self.title();
        let drawn = modal(
            ctx,
            "w10e-variables",
            title,
            None,
            DialogWidth::Standard,
            |ui| self.body(ui),
        );
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => {
                    self.request = VariablesRequest::Save;
                    self.confirm()
                        .map_or(DialogOutcome::Open, DialogOutcome::Confirmed)
                }
                DialogButton::Extra(i) => {
                    // Data Sets page extras: 0 = Export, 1 = Preview.
                    self.request = if i == 0 {
                        VariablesRequest::Export(self.format)
                    } else {
                        VariablesRequest::Preview(self.selected_set)
                    };
                    self.confirm()
                        .map_or(DialogOutcome::Open, DialogOutcome::Confirmed)
                }
            };
        }
        outcome
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        ui.horizontal(|ui| {
            for (page, key) in [
                (VariablesPage::Define, "ui.variables.define"),
                (VariablesPage::DataSets, "ui.variables.sets"),
            ] {
                if ui.selectable_label(self.page == page, tr(key)).clicked() {
                    self.page = page;
                }
            }
        });
        match self.page {
            VariablesPage::Define => self.define_page(ui),
            VariablesPage::DataSets => self.sets_page(ui),
        }
        let blocked = self.blocked_reason();
        match self.page {
            VariablesPage::Define => action_row(ui, tr("ui.variables.ok"), blocked.as_deref(), &[]),
            VariablesPage::DataSets => {
                let no_sets = self.sets.is_empty().then(|| tr("ui.variables.no.sets"));
                let extras = [
                    (tr("ui.variables.export"), no_sets),
                    (tr("ui.variables.preview"), no_sets),
                ];
                super::chrome::action_row_with_extras(
                    ui,
                    tr("ui.variables.ok"),
                    blocked.as_deref(),
                    &extras,
                )
            }
        }
    }

    fn define_page(&mut self, ui: &mut egui::Ui) {
        caption(ui, tr("ui.variables.define.subtitle"));
        if self.layers.is_empty() {
            caption(ui, tr("ui.variables.no.layers"));
            return;
        }
        egui::ScrollArea::vertical()
            .max_height(sizes::list_max_height())
            .show(ui, |ui| {
                for (i, layer) in self.layers.iter().enumerate() {
                    let b = &mut self.bindings[i];
                    design::section_header(ui, &layer.name);
                    if layer.is_text {
                        ui.horizontal(|ui| {
                            checkbox_row(ui, tr("ui.variables.text"), &mut b.text_on);
                            ui.add_enabled(
                                b.text_on,
                                egui::TextEdit::singleline(&mut b.text_name)
                                    .id(egui::Id::new(("w10e-var-text", i)))
                                    .desired_width(sizes::text_field_name()),
                            );
                        });
                    }
                    ui.horizontal(|ui| {
                        checkbox_row(ui, tr("ui.variables.visibility"), &mut b.visibility_on);
                        ui.add_enabled(
                            b.visibility_on,
                            egui::TextEdit::singleline(&mut b.visibility_name)
                                .id(egui::Id::new(("w10e-var-vis", i)))
                                .desired_width(sizes::text_field_name()),
                        );
                    });
                }
            });
    }

    fn sets_page(&mut self, ui: &mut egui::Ui) {
        caption(ui, tr("ui.variables.sets.subtitle"));
        if ui.button(tr("ui.variables.import")).clicked() {
            self.csv_request = true;
        }
        if let Some(error) = &self.csv_error {
            warning(ui, error.clone());
        }
        if self.sets.is_empty() {
            caption(ui, tr("ui.variables.no.sets"));
            return;
        }
        let options: Vec<usize> = (0..self.sets.len()).collect();
        let names: Vec<String> = self.sets.iter().map(|s| s.name.clone()).collect();
        design::inspector_field(ui, tr("ui.variables.set"), |ui| {
            combo(
                ui,
                "w10e-var-set",
                &mut self.selected_set,
                &options,
                |i| names.get(i).cloned().unwrap_or_default(),
                |_| None,
            );
        });
        if let Some(set) = self.sets.get(self.selected_set) {
            egui::ScrollArea::vertical()
                .max_height(sizes::list_max_height())
                .show(ui, |ui| {
                    for (k, v) in &set.values {
                        design::inspector_field(ui, k, |ui| {
                            ui.label(v.clone());
                        });
                    }
                });
        }
        design::inspector_field(ui, tr("ui.batch.format"), |ui| {
            combo(
                ui,
                "w10e-var-format",
                &mut self.format,
                &BatchFormat::ALL,
                BatchFormat::label,
                |_| None,
            );
        });
    }
}

/// A variable name from a layer name: letters, digits and `_`, never empty.
fn default_name(layer: &str) -> String {
    let name: String = layer
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    let name = name.trim_matches('_').to_string();
    if name.is_empty() {
        "variable".to_string()
    } else {
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layers() -> (Vec<VariableLayer>, LayerId, LayerId) {
        let (title, badge) = (LayerId::new(), LayerId::new());
        (
            vec![
                VariableLayer {
                    layer: title,
                    name: "Title".into(),
                    is_text: true,
                },
                VariableLayer {
                    layer: badge,
                    name: "Badge".into(),
                    is_text: false,
                },
            ],
            title,
            badge,
        )
    }

    #[test]
    fn the_csv_reader_handles_quotes_commas_and_line_breaks() {
        let rows = parse_csv("a,b\r\n\"x, y\",\"say \"\"hi\"\"\"\n\n\"two\nlines\",z").unwrap();
        assert_eq!(
            rows,
            vec![
                vec!["a".to_string(), "b".to_string()],
                vec!["x, y".to_string(), "say \"hi\"".to_string()],
                vec!["two\nlines".to_string(), "z".to_string()],
            ]
        );
        assert!(parse_csv("\"open").is_err());
    }

    #[test]
    fn data_sets_follow_the_header_and_refuse_unknown_or_ragged_rows() {
        let (layers, _, _) = layers();
        let mut dialog = VariablesDialog::new(VariablesPage::Define, layers, &[], Vec::new());
        dialog.bind(0, VariableKind::Text, Some("title"));
        dialog.bind(1, VariableKind::Visibility, Some("badge"));
        // A text variable cannot bind to a non-text layer.
        dialog.bind(1, VariableKind::Text, Some("nope"));
        let defs = dialog.defs();
        assert_eq!(defs.len(), 2);
        let sets = data_sets_from_csv("title,badge\nHello,true\nWorld,false\n", &defs).unwrap();
        assert_eq!(sets.len(), 2);
        assert_eq!(sets[1].value("title"), Some("World"));
        assert!(!visibility_value(sets[1].value("badge").unwrap()));
        assert!(visibility_value("Visible"));
        assert!(data_sets_from_csv("other\nx\n", &defs).is_err());
        assert!(data_sets_from_csv("title,badge\nonly-one\n", &defs).is_err());
        assert!(data_sets_from_csv("title,badge\n", &defs).is_err());
        assert_eq!(dialog.load_csv("title,badge\nA,1\nB,0\nC,1").unwrap(), 3);
        dialog.set_request(VariablesRequest::Preview(1));
        let spec = dialog.confirm().expect("previewable");
        assert_eq!(spec.request, VariablesRequest::Preview(1));
        assert_eq!(spec.sets.len(), 3);
    }

    #[test]
    fn duplicate_or_blank_names_block_and_no_sets_blocks_an_export() {
        let (layers, title, _) = layers();
        let defs = vec![VariableDef {
            name: "t".into(),
            kind: VariableKind::Text,
            layer: title,
        }];
        let mut dialog = VariablesDialog::new(VariablesPage::DataSets, layers, &defs, Vec::new());
        assert_eq!(dialog.defs(), defs, "opens on the document's definitions");
        dialog.bind(1, VariableKind::Visibility, Some("t"));
        assert!(dialog.blocked_reason().is_some(), "two variables named t");
        dialog.bind(1, VariableKind::Visibility, Some(" "));
        assert!(dialog.blocked_reason().is_some(), "a blank name");
        dialog.bind(1, VariableKind::Visibility, None);
        assert_eq!(dialog.blocked_reason(), None);
        dialog.set_request(VariablesRequest::Export(BatchFormat::Png));
        assert!(dialog.blocked_reason().is_some(), "nothing to export");
    }

    #[test]
    fn it_draws_both_pages_in_both_appearances() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let (layers, _, _) = layers();
            let mut dialog =
                VariablesDialog::new(VariablesPage::Define, layers.clone(), &[], Vec::new());
            assert!(dialog.show(ctx).is_open());
            let mut sets = VariablesDialog::new(
                VariablesPage::DataSets,
                layers,
                &[],
                vec![DataSet {
                    name: "Data Set 1".into(),
                    values: vec![("a".into(), "b".into())],
                }],
            );
            assert!(sets.show(ctx).is_open());
        });
    }
}
