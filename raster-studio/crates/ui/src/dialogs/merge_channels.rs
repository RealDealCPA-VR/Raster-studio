//! W13-N: Image ▸ Merge Channels… — which open grayscale document becomes
//! the red, the green and the blue channel of the merged RGB document.
//!
//! The application lists the candidates (open grayscale documents of one
//! size) and builds the document from the confirmed assignment; this module
//! owns only the question.

use egui::Context;

use super::chrome::{
    action_row, caption, modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::controls::combo;
use crate::strings::tr;

/// What OK hands the application: for red, green and blue, the index of
/// the candidate document that supplies it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MergeChannelsSpec {
    pub sources: [usize; 3],
}

/// Image ▸ Merge Channels….
#[derive(Clone, Debug, PartialEq)]
pub struct MergeChannelsDialog {
    /// The candidate documents' titles, in the order the indices count.
    candidates: Vec<String>,
    sources: [usize; 3],
}

/// Stable ids for a headless test.
pub mod ids {
    /// The channel's source picker (0 red, 1 green, 2 blue).
    pub fn channel(c: usize) -> egui::Id {
        egui::Id::new(("raster-merge-channels", c))
    }
}

impl MergeChannelsDialog {
    /// Over `candidates` (titles). Red, green and blue start on the first
    /// three, in order — or the last one, when there are fewer.
    pub fn new(candidates: Vec<String>) -> Self {
        let last = candidates.len().saturating_sub(1);
        Self {
            candidates,
            sources: [0, 1.min(last), 2.min(last)],
        }
    }

    pub fn candidates(&self) -> &[String] {
        &self.candidates
    }

    pub fn sources(&self) -> [usize; 3] {
        self.sources
    }

    /// Assign candidate `source` to channel `channel` (0 red, 1 green,
    /// 2 blue); out-of-range values are ignored.
    pub fn set_source(&mut self, channel: usize, source: usize) {
        if channel < 3 && source < self.candidates.len() {
            self.sources[channel] = source;
        }
    }

    fn confirm(&self) -> Option<MergeChannelsSpec> {
        (!self.candidates.is_empty()).then_some(MergeChannelsSpec {
            sources: self.sources,
        })
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<MergeChannelsSpec> {
        let keys = DialogKeys::read(ctx);
        if keys.cancel {
            return DialogOutcome::Cancelled;
        }
        if keys.confirm {
            if let Some(spec) = self.confirm() {
                return DialogOutcome::Confirmed(spec);
            }
        }
        let drawn = modal(
            ctx,
            "w13n-merge-channels",
            tr("ui.merge_channels.title"),
            None,
            DialogWidth::Standard,
            |ui| self.body(ui),
        );
        match drawn {
            Some(Some(DialogButton::Cancel)) => DialogOutcome::Cancelled,
            Some(Some(DialogButton::Confirm)) => self
                .confirm()
                .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
            _ => DialogOutcome::Open,
        }
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        caption(ui, tr("ui.merge_channels.subtitle"));
        let options: Vec<usize> = (0..self.candidates.len()).collect();
        let labels = [
            tr("ui.merge_channels.red"),
            tr("ui.merge_channels.green"),
            tr("ui.merge_channels.blue"),
        ];
        for (c, label) in labels.into_iter().enumerate() {
            design::inspector_field(ui, label, |ui| {
                let mut pick = self.sources[c];
                let changed = combo(
                    ui,
                    ids::channel(c),
                    &mut pick,
                    &options,
                    |i| self.candidates[i].clone(),
                    |_| None,
                );
                if changed {
                    self.sources[c] = pick;
                }
            });
        }
        action_row(ui, tr("ui.merge_channels.ok"), None, &[])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_string_resolves_through_the_catalogue() {
        for key in [
            "ui.merge_channels.title",
            "ui.merge_channels.subtitle",
            "ui.merge_channels.red",
            "ui.merge_channels.green",
            "ui.merge_channels.blue",
            "ui.merge_channels.ok",
        ] {
            assert!(!tr(key).is_empty(), "{key} is not in the catalogue");
        }
    }

    #[test]
    fn channels_start_on_the_first_three_and_take_any_candidate() {
        let mut d = MergeChannelsDialog::new(vec!["a".into(), "b".into(), "c".into()]);
        assert_eq!(d.sources(), [0, 1, 2]);
        d.set_source(0, 2);
        d.set_source(2, 0);
        d.set_source(1, 9);
        assert_eq!(d.sources(), [2, 1, 0]);
        let drawn = super::super::chrome::test_support::frame(|ctx| d.show(ctx));
        assert_eq!(drawn, DialogOutcome::Open);
    }
}
