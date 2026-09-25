//! W13X-4: Channels ▸ New Spot Channel… — the name, the ink colour and the
//! solidity of a new spot channel.
//!
//! The Channels panel menu opens it (`view::docks`' `channels_menu`) and
//! holds it in [`crate::panels::channels::ChannelsState::spot_dialog`]; OK
//! hands back a [`SpotChannelSpec`], which the panel turns into one
//! `editor_core::Command::SetSpotChannels` step.

/// The ink a new spot channel starts with in this build: a pure red, which
/// the dialog lets the user change before OK.
pub const DEFAULT_SPOT_INK: [u8; 3] = [255, 0, 0];

/// What the New Spot Channel dialog's OK hands the panel.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SpotChannelSpec {
    pub name: String,
    pub ink: [u8; 3],
    /// 0..=100.
    pub solidity: u8,
}

/// Channels ▸ New Spot Channel…: the name, the ink colour and the solidity.
#[derive(Clone, PartialEq, Debug)]
pub struct SpotChannelDialog {
    name: String,
    ink: [u8; 3],
    solidity: u8,
}

impl SpotChannelDialog {
    /// Opens on `name` (the next free "Spot Color N"), [`DEFAULT_SPOT_INK`]
    /// and Photoshop's 0% solidity.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ink: DEFAULT_SPOT_INK,
            solidity: editor_core::spot::DEFAULT_SOLIDITY,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_name(&mut self, name: impl Into<String>) {
        self.name = name.into();
    }

    pub fn ink(&self) -> [u8; 3] {
        self.ink
    }

    pub fn set_ink(&mut self, ink: [u8; 3]) {
        self.ink = ink;
    }

    pub fn solidity(&self) -> u8 {
        self.solidity
    }

    /// Clamped to 100.
    pub fn set_solidity(&mut self, solidity: u8) {
        self.solidity = solidity.min(100);
    }

    fn confirm(&self) -> Option<SpotChannelSpec> {
        let name = self.name.trim();
        (!name.is_empty()).then(|| SpotChannelSpec {
            name: name.to_string(),
            ink: self.ink,
            solidity: self.solidity.min(100),
        })
    }

    /// Draw one frame; Enter confirms and Escape cancels, as in every dialog.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
    ) -> crate::dialogs::chrome::DialogOutcome<SpotChannelSpec> {
        use crate::dialogs::chrome::{modal, DialogButton, DialogKeys, DialogOutcome, DialogWidth};
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
            "w13x4-new-spot-channel",
            crate::strings::tr("ui.spot_channel.title"),
            None,
            DialogWidth::Standard,
            |ui| self.body(ui),
        );
        // Hold the keyboard while no field of the dialog has it.
        let sink = crate::panels::channels::spot_ids::keyboard_sink();
        egui::Area::new(sink.with("area"))
            .fixed_pos(egui::Pos2::ZERO)
            .interactable(false)
            .show(ctx, |ui| {
                let at = egui::Rect::from_min_size(ui.max_rect().min, egui::Vec2::ZERO);
                let _ = ui.interact(at, sink, egui::Sense::hover());
            });
        if ctx.memory(|m| m.focused().is_none()) {
            ctx.memory_mut(|m| m.request_focus(sink));
        }
        match drawn {
            Some(Some(DialogButton::Cancel)) => DialogOutcome::Cancelled,
            Some(Some(DialogButton::Confirm)) => self
                .confirm()
                .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
            _ => DialogOutcome::Open,
        }
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<crate::dialogs::chrome::DialogButton> {
        use crate::dialogs::controls::{from_byte, integer, numeric, swatch_readonly};
        use crate::strings::tr;
        crate::dialogs::chrome::caption(ui, tr("ui.spot_channel.subtitle"));
        design::inspector_field(ui, tr("ui.spot_channel.name"), |ui| {
            ui.add(
                egui::TextEdit::singleline(&mut self.name)
                    .desired_width(crate::dialogs::sizes::text_field_wide()),
            );
        });
        design::inspector_field(ui, tr("ui.spot_channel.ink"), |ui| {
            for c in 0..3 {
                let mut v = i64::from(self.ink[c]);
                if integer(ui, &mut v, 0..=255).changed() {
                    self.ink[c] = v.clamp(0, 255) as u8;
                }
            }
            let rgba = [
                from_byte(self.ink[0]),
                from_byte(self.ink[1]),
                from_byte(self.ink[2]),
                from_byte(u8::MAX),
            ];
            swatch_readonly(
                ui,
                crate::panels::channels::spot_ids::dialog_swatch(),
                rgba,
                crate::dialogs::sizes::swatch(),
            );
        });
        design::inspector_field(ui, tr("ui.spot_channel.solidity"), |ui| {
            let mut v = f64::from(self.solidity);
            if numeric(ui, &mut v, 0.0..=100.0, 0, "%").changed() {
                self.solidity = v.round().clamp(0.0, 100.0) as u8;
            }
        });
        let blocked = self
            .name
            .trim()
            .is_empty()
            .then(|| tr("ui.spot_channel.no_name"));
        crate::dialogs::chrome::action_row(ui, tr("ui.spot_channel.ok"), blocked, &[])
    }
}
