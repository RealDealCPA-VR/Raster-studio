//! Help ▸ About Raster Studio — a real window, not a status line.
//!
//! The dialog shows three facts the application hands it at open time: the
//! product-and-version line (the executable's stamp, `app_shell::about_line`),
//! where the licence lives, and where the third-party notices live. It asks
//! nothing, so it has one button and no [`super::chrome::Dialog`] impl: that
//! trait is the confirm/cancel contract, and a window with nothing to confirm
//! would have to invent a [`super::action::DialogAction`] to satisfy it.
//! Escape, Enter and the Close button all dismiss it.

use egui::Context;

use super::chrome::{caption, hairline, modal, DialogKeys, DialogWidth};
use crate::strings::tr;
use design::tokens::Space;

/// Help ▸ About Raster Studio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AboutDialog {
    version_line: String,
    licence: String,
    notices: String,
}

impl AboutDialog {
    /// `version_line` is the product and version as one line; `licence` and
    /// `notices` are the pointers the window shows under it.
    pub fn new(
        version_line: impl Into<String>,
        licence: impl Into<String>,
        notices: impl Into<String>,
    ) -> Self {
        Self {
            version_line: version_line.into(),
            licence: licence.into(),
            notices: notices.into(),
        }
    }

    /// The product-and-version line the window shows.
    pub fn version_line(&self) -> &str {
        &self.version_line
    }

    /// The licence pointer the window shows.
    pub fn licence(&self) -> &str {
        &self.licence
    }

    /// The third-party notices pointer the window shows.
    pub fn notices(&self) -> &str {
        &self.notices
    }

    /// The window's title.
    pub fn title(&self) -> &'static str {
        tr("ui.about.title")
    }

    /// Draw one frame. Returns `true` when the window was dismissed — by
    /// Escape, by Enter, or by the Close button.
    pub fn show(&mut self, ctx: &Context) -> bool {
        let keys = DialogKeys::read(ctx);
        let mut dismissed = keys.cancel || keys.confirm;
        let drawn = modal(
            ctx,
            "about",
            self.title(),
            Some(tr("ui.about.tagline")),
            DialogWidth::Standard,
            |ui| {
                ui.label(egui::RichText::new(self.version_line.as_str()).strong());
                hairline(ui);
                design::inspector_field(ui, "Licence", |ui| {
                    ui.label(self.licence.as_str());
                });
                design::inspector_field(ui, tr("ui.about.third.party.notices"), |ui| {
                    ui.label(self.notices.as_str());
                });
                ui.add_space(Space::Small.pt());
                caption(ui, tr("ui.about.tagline"));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    design::primary_button(ui, "Close").clicked()
                })
                .inner
            },
        );
        if drawn == Some(true) {
            dismissed = true;
        }
        dismissed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_shows_what_it_was_given() {
        let dialog = AboutDialog::new("Raster Studio 9.9.9 (abc1234)", "LICENSES/", "notices.md");
        assert_eq!(dialog.version_line(), "Raster Studio 9.9.9 (abc1234)");
        assert_eq!(dialog.licence(), "LICENSES/");
        assert_eq!(dialog.notices(), "notices.md");
        assert_eq!(dialog.title(), "About Raster Studio");
    }

    #[test]
    fn it_draws_in_both_appearances_and_stays_open_until_dismissed() {
        super::super::chrome::test_support::frame_both_themes(|ctx| {
            let mut dialog = AboutDialog::new("Raster Studio 0.0.0", "LICENSES/", "x.md");
            assert!(!dialog.show(ctx), "no key was pressed, so it stays open");
        });
    }

    #[test]
    fn escape_dismisses_it() {
        let ctx = Context::default();
        design::apply_theme(&ctx, design::Theme::Dark);
        let mut dialog = AboutDialog::new("Raster Studio 0.0.0", "LICENSES/", "x.md");
        let input = egui::RawInput {
            events: vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::default(),
            }],
            ..Default::default()
        };
        let mut dismissed = false;
        let _ = ctx.run(input, |ctx| dismissed = dialog.show(ctx));
        assert!(dismissed);
    }
}
