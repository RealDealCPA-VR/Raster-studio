//! W9-K: Layer ▸ Text ▸ Warp Text… — the style, the bend and the two
//! distortions of the active text layer's live warp.
//!
//! The dialog edits a copy of the text layer and confirms to one
//! [`Command::SetLayerKind`] carrying the whole new warp, so the change is one
//! undo step that travels the same road every other text edit does. The text
//! stays editable: the compositor bends the glyph outlines on every render.
//! Cancel writes nothing. An unchanged warp is refused with a reason rather
//! than recording an undo step that changes nothing.

use editor_core::Command;
use egui::Context;
use layer_model::text::{TextLayer, TextWarp, WarpStyle};
use layer_model::{LayerId, LayerKind};

use super::action::DialogAction;
use super::chrome::{
    action_row, modal, resolve, Dialog, DialogButton, DialogKeys, DialogOutcome, DialogWidth,
};
use super::controls;
use crate::strings::tr;

/// The style choices, None first (it clears the warp). W13X-5: Custom last -
/// its mesh handles are dragged on the canvas (`app-shell`'s `warp_custom`).
const STYLES: [WarpStyle; 15] = [
    WarpStyle::None,
    WarpStyle::Arc,
    WarpStyle::ArcLower,
    WarpStyle::ArcUpper,
    WarpStyle::Arch,
    WarpStyle::Bulge,
    WarpStyle::Flag,
    WarpStyle::Wave,
    WarpStyle::Fish,
    WarpStyle::Rise,
    WarpStyle::Fisheye,
    WarpStyle::Inflate,
    WarpStyle::Squeeze,
    WarpStyle::Twist,
    WarpStyle::Custom,
];

/// Layer ▸ Text ▸ Warp Text….
#[derive(Debug, Clone, PartialEq)]
pub struct WarpTextDialog {
    layer: LayerId,
    text: TextLayer,
    style: WarpStyle,
    /// The three numbers in whole percent, `-100..=100`, as the fields show
    /// them.
    bend: i32,
    horizontal: i32,
    vertical: i32,
}

/// A fraction as the dialog's whole percent, clamped to `-100..=100`.
fn percent(v: f32) -> i32 {
    if v.is_finite() {
        (v.clamp(-1.0, 1.0) * 100.0).round() as i32
    } else {
        0
    }
}

impl WarpTextDialog {
    /// Over the text layer `layer`, whose payload is `text`. A layer with no
    /// warp opens on Arc at the default 50 % bend, so Enter warps at once
    /// (Photoshop opens its dialog on None; the parity choice here is that the
    /// dialog's primary action does something).
    pub fn new(layer: LayerId, text: TextLayer) -> Self {
        let start = if text.warp.is_active() {
            text.warp
        } else {
            TextWarp::new(WarpStyle::Arc)
        };
        Self {
            layer,
            style: start.style,
            bend: percent(start.bend),
            horizontal: percent(start.horizontal),
            vertical: percent(start.vertical),
            text,
        }
    }

    pub fn layer(&self) -> LayerId {
        self.layer
    }

    pub fn set_style(&mut self, style: WarpStyle) {
        self.style = style;
    }

    /// Set the bend and the two distortions, in whole percent (clamped).
    pub fn set_amounts(&mut self, bend: i32, horizontal: i32, vertical: i32) {
        self.bend = bend.clamp(-100, 100);
        self.horizontal = horizontal.clamp(-100, 100);
        self.vertical = vertical.clamp(-100, 100);
    }

    /// The warp a confirmation writes. None is the default "no warp" value
    /// whatever the fields hold, so a cleared warp serialises as before.
    pub fn warp(&self) -> TextWarp {
        if self.style == WarpStyle::None {
            return TextWarp::default();
        }
        TextWarp {
            style: self.style,
            bend: self.bend as f32 / 100.0,
            horizontal: self.horizontal as f32 / 100.0,
            vertical: self.vertical as f32 / 100.0,
            // W13X-5: Custom keeps the layer's dragged mesh (flat when it has
            // none yet); the parametric styles carry none.
            mesh: (self.style == WarpStyle::Custom)
                .then_some(self.text.warp.mesh)
                .flatten(),
        }
    }

    /// Draw one frame and fold the keyboard and the action row into one
    /// outcome.
    pub fn show(&mut self, ctx: &Context) -> DialogOutcome<DialogAction> {
        let keys = DialogKeys::read(ctx);
        let mut outcome = resolve(self, keys);
        let drawn = modal(
            ctx,
            "warp-text",
            self.title(),
            None,
            DialogWidth::Narrow,
            |ui| self.body(ui),
        );
        if let Some(Some(button)) = drawn {
            outcome = match button {
                DialogButton::Cancel => DialogOutcome::Cancelled,
                DialogButton::Confirm => self
                    .confirm()
                    .map_or(DialogOutcome::Open, DialogOutcome::Confirmed),
                DialogButton::Extra(_) => DialogOutcome::Open,
            };
        }
        outcome
    }

    fn body(&mut self, ui: &mut egui::Ui) -> Option<DialogButton> {
        design::inspector_field(ui, tr("ui.warp_text.style"), |ui| {
            controls::combo(
                ui,
                "warp-text-style",
                &mut self.style,
                &STYLES,
                |s| s.label().to_string(),
                |_| None,
            );
        });
        // W13X-5: Custom has no bend: its shape is the mesh on the canvas.
        let custom = self.style == WarpStyle::Custom;
        if custom {
            super::chrome::caption(ui, tr("ui.warp_text.custom_hint"));
        }
        let active = self.style != WarpStyle::None && !custom;
        ui.add_enabled_ui(active, |ui| {
            for (key, value) in [
                ("ui.warp_text.bend", &mut self.bend),
                ("ui.warp_text.horizontal", &mut self.horizontal),
                ("ui.warp_text.vertical", &mut self.vertical),
            ] {
                design::inspector_field(ui, tr(key), |ui| {
                    ui.add(egui::Slider::new(value, -100..=100).suffix("%"));
                });
            }
        });
        action_row(
            ui,
            self.confirm_label(),
            self.blocked_reason().as_deref(),
            &[],
        )
    }
}

impl Dialog for WarpTextDialog {
    fn title(&self) -> &'static str {
        tr("ui.warp_text.title")
    }

    fn confirm_label(&self) -> &'static str {
        tr("ui.warp_text.confirm")
    }

    fn confirm(&self) -> Option<DialogAction> {
        let warp = self.warp();
        (warp != self.text.warp).then(|| {
            let mut text = self.text.clone();
            text.warp = warp;
            DialogAction::Command(Box::new(Command::SetLayerKind {
                layer_id: self.layer,
                kind: Box::new(LayerKind::Text(text)),
            }))
        })
    }

    fn blocked_reason(&self) -> Option<String> {
        (self.warp() == self.text.warp).then(|| tr("ui.warp_text.unchanged").to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text() -> TextLayer {
        TextLayer {
            text: "Arc".into(),
            ..TextLayer::default()
        }
    }

    fn confirmed_warp(dialog: &WarpTextDialog) -> TextWarp {
        match dialog.confirm() {
            Some(DialogAction::Command(command)) => match *command {
                Command::SetLayerKind { layer_id, kind } => {
                    assert_eq!(layer_id, dialog.layer());
                    match *kind {
                        LayerKind::Text(t) => {
                            assert_eq!(t.text, "Arc", "the text itself is untouched");
                            t.warp
                        }
                        other => panic!("not a text kind: {other:?}"),
                    }
                }
                other => panic!("not a SetLayerKind: {other:?}"),
            },
            other => panic!("no command: {other:?}"),
        }
    }

    #[test]
    fn w9k_the_dialog_confirms_the_style_bend_and_both_distortions() {
        let mut dialog = WarpTextDialog::new(LayerId::new(), text());
        dialog.set_style(WarpStyle::Flag);
        dialog.set_amounts(-30, 40, -250);
        let warp = confirmed_warp(&dialog);
        assert_eq!(warp.style, WarpStyle::Flag);
        assert!((warp.bend + 0.30).abs() < 1e-6, "{warp:?}");
        assert!((warp.horizontal - 0.40).abs() < 1e-6, "{warp:?}");
        assert!((warp.vertical + 1.0).abs() < 1e-6, "clamped: {warp:?}");
    }

    #[test]
    fn w9k_an_unwarped_layer_opens_on_arc_and_enter_warps_it() {
        let dialog = WarpTextDialog::new(LayerId::new(), text());
        assert!(dialog.blocked_reason().is_none());
        assert_eq!(confirmed_warp(&dialog), TextWarp::new(WarpStyle::Arc));
    }

    #[test]
    fn w9k_none_clears_the_warp_and_an_unchanged_warp_is_refused() {
        let mut warped = text();
        warped.warp = TextWarp {
            style: WarpStyle::Wave,
            bend: 0.25,
            horizontal: 0.0,
            vertical: 0.5,
            mesh: None,
        };
        let mut dialog = WarpTextDialog::new(LayerId::new(), warped);
        assert!(dialog.confirm().is_none(), "opening on the stored warp");
        assert!(dialog.blocked_reason().is_some());
        dialog.set_style(WarpStyle::None);
        assert_eq!(confirmed_warp(&dialog), TextWarp::default());
    }

    /// W13X-5: Custom is offered, confirms the Custom style, and keeps the
    /// mesh already dragged on the canvas; a parametric style drops it.
    #[test]
    fn w13x5_custom_is_offered_and_keeps_the_layers_mesh() {
        assert!(STYLES.contains(&WarpStyle::Custom));
        let mut mesh = text_engine::warp::FLAT_MESH;
        mesh[15] = [1.2, 1.4];
        let mut warped = text();
        warped.warp = TextWarp {
            style: WarpStyle::Custom,
            mesh: Some(mesh),
            ..TextWarp::default()
        };
        let mut dialog = WarpTextDialog::new(LayerId::new(), warped);
        assert!(dialog.confirm().is_none(), "opening on the stored mesh");
        dialog.set_style(WarpStyle::Arc);
        assert_eq!(confirmed_warp(&dialog).mesh, None);
        dialog.set_style(WarpStyle::Custom);
        assert!(dialog.confirm().is_none(), "back to the stored mesh");
        let mut fresh = WarpTextDialog::new(LayerId::new(), text());
        fresh.set_style(WarpStyle::Custom);
        let warp = confirmed_warp(&fresh);
        assert_eq!(warp.style, WarpStyle::Custom);
        assert_eq!(warp.mesh, None, "a new Custom warp starts flat");
    }
}
