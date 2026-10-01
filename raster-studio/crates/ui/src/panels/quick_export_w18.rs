//! W18-I: the Quick Export window — the format (PNG or SVG) and the scale
//! (1x to 4x, Photopea's "Scale for exported files") the selected layers
//! are written at, each alone.
//!
//! File ▸ Export ▸ Quick Export Selected Layers… and the Move options bar's
//! Quick Export button open it (the application holds it,
//! `app_shell::tool_input::quick_export`); Export… hands back a
//! [`QuickExportChoice`], which the application writes after asking where.
//! The window opens on the last choice.

use crate::dialogs::chrome::DialogOutcome;

/// The file format Quick Export writes.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum QuickExportFormat {
    #[default]
    Png,
    /// Shapes as paths, text as text, the rest embedded; resolution-free,
    /// so it is written at canvas size whatever the scale.
    Svg,
}

impl QuickExportFormat {
    pub const ALL: [QuickExportFormat; 2] = [QuickExportFormat::Png, QuickExportFormat::Svg];

    /// The file extension, which is also the segment's label in capitals.
    pub fn extension(self) -> &'static str {
        match self {
            QuickExportFormat::Png => "png",
            QuickExportFormat::Svg => "svg",
        }
    }

    fn label(self) -> &'static str {
        match self {
            QuickExportFormat::Png => "PNG",
            QuickExportFormat::Svg => "SVG",
        }
    }
}

/// The scales the window offers: Photopea's Move bar "Scale for exported
/// files" set, 1x to 4x.
/// (Suffixed literals: four bare floats read as a colour to the style gate.)
pub const SCALES: [f32; 4] = [1_f32, 2_f32, 3_f32, 4_f32];

/// The label of a scale: `2x`.
pub fn scale_label(scale: f32) -> String {
    if (scale - scale.round()).abs() < f32::EPSILON {
        format!("{}x", scale.round() as i32)
    } else {
        format!("{scale}x")
    }
}

/// What Export… hands the application.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct QuickExportChoice {
    pub format: QuickExportFormat,
    /// One of [`SCALES`]; `1.0` is canvas size.
    pub scale: f32,
}

impl Default for QuickExportChoice {
    fn default() -> Self {
        Self {
            format: QuickExportFormat::Png,
            scale: 1.0,
        }
    }
}

/// The Quick Export window.
#[derive(Clone, PartialEq, Debug)]
pub struct QuickExportDialog {
    format: usize,
    scale: usize,
    /// How many layers the export writes (the selected ones).
    layers: usize,
}

impl QuickExportDialog {
    /// Opens on `last` for an export of `layers` layers.
    pub fn new(layers: usize, last: QuickExportChoice) -> Self {
        let format = QuickExportFormat::ALL
            .iter()
            .position(|f| *f == last.format)
            .unwrap_or(0);
        let scale = SCALES
            .iter()
            .position(|s| (*s - last.scale).abs() < f32::EPSILON)
            .unwrap_or(0);
        Self {
            format,
            scale,
            layers,
        }
    }

    /// What Export… would hand back now.
    pub fn choice(&self) -> QuickExportChoice {
        QuickExportChoice {
            format: QuickExportFormat::ALL[self.format.min(1)],
            scale: SCALES[self.scale.min(SCALES.len() - 1)],
        }
    }

    /// Draw one frame; Enter exports and Escape cancels, as in every dialog.
    pub fn show(&mut self, ctx: &egui::Context) -> DialogOutcome<QuickExportChoice> {
        use crate::dialogs::chrome::{modal, DialogButton, DialogKeys, DialogWidth};
        let keys = DialogKeys::read(ctx);
        if keys.cancel {
            return DialogOutcome::Cancelled;
        }
        if keys.confirm {
            return DialogOutcome::Confirmed(self.choice());
        }
        let drawn = modal(
            ctx,
            ids::window(),
            crate::strings::tr("ui.w18.quick_export.title"),
            None,
            DialogWidth::Narrow,
            |ui| self.body(ui),
        );
        match drawn {
            Some(Some(DialogButton::Cancel)) => DialogOutcome::Cancelled,
            Some(Some(DialogButton::Confirm)) => DialogOutcome::Confirmed(self.choice()),
            _ => DialogOutcome::Open,
        }
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<crate::dialogs::chrome::DialogButton> {
        use crate::strings::tr;
        let what = if self.layers > 1 {
            tr("ui.w18.quick_export.many").replace("{n}", &self.layers.to_string())
        } else {
            tr("ui.w18.quick_export.one").to_string()
        };
        crate::dialogs::chrome::caption(ui, what);
        design::inspector_field(ui, tr("ui.w18.quick_export.format"), |ui| {
            let labels: Vec<&str> = QuickExportFormat::ALL.iter().map(|f| f.label()).collect();
            let _ = design::segmented_control(ui, ids::format(), &mut self.format, &labels);
        });
        let svg = self.choice().format == QuickExportFormat::Svg;
        design::inspector_field(ui, tr("ui.w18.quick_export.scale"), |ui| {
            let labels: Vec<String> = SCALES.iter().map(|s| scale_label(*s)).collect();
            let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
            ui.add_enabled_ui(!svg, |ui| {
                let _ = design::segmented_control(ui, ids::scale(), &mut self.scale, &labels);
            })
            .response
            .on_disabled_hover_text(tr("ui.w18.quick_export.svg_scale"));
        });
        crate::dialogs::chrome::action_row(ui, tr("ui.w18.quick_export.ok"), None, &[])
    }
}

/// Stable ids of the window's parts.
pub mod ids {
    /// The window.
    pub fn window() -> egui::Id {
        egui::Id::new("w18-quick-export")
    }

    /// The format segments.
    pub fn format() -> egui::Id {
        egui::Id::new("w18-quick-export-format")
    }

    /// The scale segments.
    pub fn scale() -> egui::Id {
        egui::Id::new("w18-quick-export-scale")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_opens_on_the_last_choice_and_names_its_scales() {
        let last = QuickExportChoice {
            format: QuickExportFormat::Svg,
            scale: 3.0,
        };
        assert_eq!(QuickExportDialog::new(2, last).choice(), last);
        let odd = QuickExportChoice {
            format: QuickExportFormat::Png,
            scale: 7.0,
        };
        assert_eq!(
            QuickExportDialog::new(1, odd).choice().scale,
            1.0,
            "a scale it does not offer opens on 1x"
        );
        let labels: Vec<String> = SCALES.iter().map(|s| scale_label(*s)).collect();
        assert_eq!(labels, ["1x", "2x", "3x", "4x"], "Photopea's set, no 0.5x");
        let half = QuickExportChoice {
            format: QuickExportFormat::Png,
            scale: 0.5,
        };
        assert_eq!(QuickExportDialog::new(1, half).choice().scale, 1.0);
    }
}
