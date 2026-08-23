//! One frame of egui input, turned into an ordered stream of pointer samples.
//!
//! # Why this is a type and not a function
//!
//! Which button is held is a fact about the **gesture**, not about the frame.
//! The press arrives in frame *N* and the release in frame *N+40*; every
//! `PointerMoved` in between belongs to whichever button went down forty frames
//! ago, and an `Event::PointerGone` in frame *N+k* has to stand in for the
//! release that will now never come.
//!
//! Rebuilding that state from a single frame's event list gets both of those
//! wrong. The moves are attributed to the primary button whatever is actually
//! held — so a middle-button pan reports primary moves — and, far worse, the
//! synthesized release only happens when the press was in the *same* frame,
//! which is never the real case. The router's gesture is then never let go, and
//! [`crate::canvas::InputRouter`] refuses every later press as somebody else's:
//! the canvas goes permanently dead.
//!
//! So the state lives here and persists across frames. egui's own
//! `PointerState` — which does survive between frames — is used as a backstop
//! for the one case this cannot see: a release that happened while the canvas
//! was not being shown at all.

use glam::Vec2;

use super::geom::from_pos2;
use super::input::{PointerButton, PointerInput, PointerPhase};

/// `tools`' modifier set from egui's.
///
/// `command` folds into `ctrl`: on macOS the platform modifier is Cmd, and a
/// tool that checks `ctrl` means "the platform's modifier key", not "the key
/// labelled Ctrl".
pub fn modifiers_from_egui(m: egui::Modifiers) -> tools::Modifiers {
    tools::Modifiers {
        shift: m.shift,
        alt: m.alt,
        ctrl: m.ctrl || m.command,
    }
}

/// egui's pointer button to ours; the two extra buttons are not routed.
pub fn button_from_egui(b: egui::PointerButton) -> Option<PointerButton> {
    match b {
        egui::PointerButton::Primary => Some(PointerButton::Primary),
        egui::PointerButton::Secondary => Some(PointerButton::Secondary),
        egui::PointerButton::Middle => Some(PointerButton::Middle),
        egui::PointerButton::Extra1 | egui::PointerButton::Extra2 => None,
    }
}

/// Ours back to egui's, for polling egui's persistent button state.
pub const fn button_to_egui(b: PointerButton) -> egui::PointerButton {
    match b {
        PointerButton::Primary => egui::PointerButton::Primary,
        PointerButton::Secondary => egui::PointerButton::Secondary,
        PointerButton::Middle => egui::PointerButton::Middle,
    }
}

/// What one frame of input produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FrameSamples {
    /// The samples, in the order they arrived.
    pub samples: Vec<PointerInput>,
    /// The pointer left the window this frame. Any gesture still running after
    /// the samples below have been routed has to be let go, not left hanging.
    pub pointer_gone: bool,
}

/// The held button and the last known position, carried between frames.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PointerTracker {
    held: Option<PointerButton>,
    last_pt: Option<Vec2>,
}

impl PointerTracker {
    /// The button currently held, as far as the canvas has seen.
    pub fn held(&self) -> Option<PointerButton> {
        self.held
    }

    /// The last position a sample was produced at.
    pub fn last_pt(&self) -> Option<Vec2> {
        self.last_pt
    }

    /// Forget everything. For a caller that has cancelled the gesture itself.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Turn one frame of egui input into samples, in order.
    pub fn frame(&mut self, input: &egui::InputState, pressure: f32) -> FrameSamples {
        let mut out = FrameSamples::default();
        for event in &input.events {
            match event {
                egui::Event::PointerButton {
                    pos,
                    button,
                    pressed,
                    modifiers,
                } => {
                    let Some(b) = button_from_egui(*button) else {
                        continue;
                    };
                    let at = from_pos2(*pos);
                    self.last_pt = Some(at);
                    if *pressed {
                        self.held = Some(b);
                    } else if self.held == Some(b) {
                        self.held = None;
                    }
                    out.samples.push(PointerInput {
                        phase: if *pressed {
                            PointerPhase::Down
                        } else {
                            PointerPhase::Up
                        },
                        button: b,
                        pos_pt: at,
                        pressure,
                        modifiers: modifiers_from_egui(*modifiers),
                    });
                }
                egui::Event::PointerMoved(pos) => {
                    let at = from_pos2(*pos);
                    self.last_pt = Some(at);
                    out.samples.push(PointerInput {
                        phase: PointerPhase::Move,
                        // The button that is *actually* down, however many
                        // frames ago it went down.
                        button: self.held.unwrap_or(PointerButton::Primary),
                        pos_pt: at,
                        pressure,
                        modifiers: modifiers_from_egui(input.modifiers),
                    });
                }
                egui::Event::PointerGone => {
                    // Leaving the window releases whatever was held, wherever
                    // it was last seen — even if that press was frames ago.
                    out.pointer_gone = true;
                    if let (Some(b), Some(at)) = (self.held.take(), self.last_pt) {
                        out.samples.push(PointerInput {
                            phase: PointerPhase::Up,
                            button: b,
                            pos_pt: at,
                            pressure,
                            modifiers: modifiers_from_egui(input.modifiers),
                        });
                    }
                }
                _ => {}
            }
        }

        // The backstop: a release that happened while the canvas was not on
        // screen leaves this tracker holding a button that egui knows is up.
        // Synthesize the release rather than let a gesture run for ever.
        if let Some(b) = self.held {
            if !input.pointer.button_down(button_to_egui(b)) {
                self.held = None;
                if let Some(at) = self.last_pt {
                    out.samples.push(PointerInput {
                        phase: PointerPhase::Up,
                        button: b,
                        pos_pt: at,
                        pressure,
                        modifiers: modifiers_from_egui(input.modifiers),
                    });
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(x: f32, y: f32, button: egui::PointerButton) -> egui::Event {
        egui::Event::PointerButton {
            pos: egui::pos2(x, y),
            button,
            pressed: true,
            modifiers: egui::Modifiers::default(),
        }
    }

    fn release(x: f32, y: f32, button: egui::PointerButton) -> egui::Event {
        egui::Event::PointerButton {
            pos: egui::pos2(x, y),
            button,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        }
    }

    /// Run one egui frame carrying `events` and collect what the tracker made
    /// of it. The tracker outlives the frame, which is the whole point.
    fn frame(
        ctx: &egui::Context,
        tracker: &mut PointerTracker,
        events: Vec<egui::Event>,
    ) -> FrameSamples {
        let mut out = FrameSamples::default();
        let _ = ctx.run(
            egui::RawInput {
                events,
                ..Default::default()
            },
            |ctx| {
                out = ctx.input(|i| tracker.frame(i, 1.0));
            },
        );
        out
    }

    #[test]
    fn a_frame_of_events_becomes_an_ordered_sample_stream() {
        let ctx = egui::Context::default();
        let mut t = PointerTracker::default();
        let out = frame(
            &ctx,
            &mut t,
            vec![
                press(10.0, 10.0, egui::PointerButton::Middle),
                egui::Event::PointerMoved(egui::pos2(20.0, 15.0)),
                release(20.0, 15.0, egui::PointerButton::Middle),
            ],
        );
        assert_eq!(out.samples.len(), 3);
        assert_eq!(out.samples[0].phase, PointerPhase::Down);
        assert_eq!(out.samples[1].phase, PointerPhase::Move);
        assert_eq!(out.samples[1].button, PointerButton::Middle);
        assert_eq!(out.samples[2].phase, PointerPhase::Up);
        assert!(!out.pointer_gone);
        assert_eq!(t.held(), None);
    }

    /// The defect this type exists for: the press and the moves are in
    /// different frames, so a per-frame `held` reports the primary button for
    /// every one of them.
    #[test]
    fn a_move_in_a_later_frame_still_belongs_to_the_button_that_is_down() {
        let ctx = egui::Context::default();
        let mut t = PointerTracker::default();
        frame(
            &ctx,
            &mut t,
            vec![press(10.0, 10.0, egui::PointerButton::Middle)],
        );
        assert_eq!(t.held(), Some(PointerButton::Middle));
        for step in 1..4 {
            let out = frame(
                &ctx,
                &mut t,
                vec![egui::Event::PointerMoved(egui::pos2(
                    10.0 + step as f32,
                    10.0,
                ))],
            );
            assert_eq!(out.samples.len(), 1);
            assert_eq!(
                out.samples[0].button,
                PointerButton::Middle,
                "frame {step}: a middle-button pan reported a primary move"
            );
        }
        let out = frame(
            &ctx,
            &mut t,
            vec![release(13.0, 10.0, egui::PointerButton::Middle)],
        );
        assert_eq!(out.samples[0].phase, PointerPhase::Up);
        assert_eq!(out.samples[0].button, PointerButton::Middle);
        assert_eq!(t.held(), None);
    }

    /// …and the other half of it: `PointerGone` arrives in a *later* frame than
    /// the press, which is the only way it ever arrives in practice.
    #[test]
    fn losing_the_pointer_a_frame_later_still_synthesizes_the_release() {
        let ctx = egui::Context::default();
        let mut t = PointerTracker::default();
        frame(
            &ctx,
            &mut t,
            vec![press(30.0, 40.0, egui::PointerButton::Primary)],
        );
        let out = frame(&ctx, &mut t, vec![egui::Event::PointerGone]);
        assert!(out.pointer_gone);
        assert_eq!(out.samples.len(), 1, "{:?}", out.samples);
        assert_eq!(out.samples[0].phase, PointerPhase::Up);
        assert_eq!(out.samples[0].button, PointerButton::Primary);
        assert_eq!(
            out.samples[0].pos_pt,
            Vec2::new(30.0, 40.0),
            "the release is synthesized where the pointer was last seen"
        );
        assert_eq!(t.held(), None);
        // A second `PointerGone` has nothing left to release.
        let again = frame(&ctx, &mut t, vec![egui::Event::PointerGone]);
        assert!(again.pointer_gone);
        assert!(again.samples.is_empty());
    }

    /// The backstop: the canvas was not drawn for the frame carrying the
    /// release, so egui saw it and this tracker did not.
    #[test]
    fn a_release_missed_while_the_canvas_was_hidden_is_recovered_from_egui() {
        let ctx = egui::Context::default();
        let mut t = PointerTracker::default();
        frame(
            &ctx,
            &mut t,
            vec![press(50.0, 60.0, egui::PointerButton::Primary)],
        );
        // egui processes the release; the canvas never asks for the samples.
        let _ = ctx.run(
            egui::RawInput {
                events: vec![release(50.0, 60.0, egui::PointerButton::Primary)],
                ..Default::default()
            },
            |_| {},
        );
        assert_eq!(t.held(), Some(PointerButton::Primary));
        let out = frame(&ctx, &mut t, Vec::new());
        assert_eq!(out.samples.len(), 1);
        assert_eq!(out.samples[0].phase, PointerPhase::Up);
        assert_eq!(t.held(), None);
        // …and nothing more is invented on the frames after that.
        assert!(frame(&ctx, &mut t, Vec::new()).samples.is_empty());
    }

    #[test]
    fn the_extra_buttons_are_dropped_and_never_become_held() {
        let ctx = egui::Context::default();
        let mut t = PointerTracker::default();
        let out = frame(
            &ctx,
            &mut t,
            vec![
                press(1.0, 1.0, egui::PointerButton::Extra1),
                egui::Event::PointerMoved(egui::pos2(2.0, 2.0)),
            ],
        );
        assert_eq!(out.samples.len(), 1);
        assert_eq!(out.samples[0].phase, PointerPhase::Move);
        assert_eq!(t.held(), None);
        assert_eq!(button_from_egui(egui::PointerButton::Extra2), None);
        assert_eq!(
            button_to_egui(PointerButton::Secondary),
            egui::PointerButton::Secondary
        );
    }

    #[test]
    fn resetting_forgets_the_gesture() {
        let ctx = egui::Context::default();
        let mut t = PointerTracker::default();
        frame(
            &ctx,
            &mut t,
            vec![press(5.0, 5.0, egui::PointerButton::Primary)],
        );
        assert!(t.held().is_some() && t.last_pt().is_some());
        t.reset();
        assert_eq!(t, PointerTracker::default());
    }

    #[test]
    fn pressure_rides_along_on_every_sample() {
        let ctx = egui::Context::default();
        let mut t = PointerTracker::default();
        let mut out = FrameSamples::default();
        let _ = ctx.run(
            egui::RawInput {
                events: vec![
                    press(1.0, 1.0, egui::PointerButton::Primary),
                    egui::Event::PointerMoved(egui::pos2(2.0, 2.0)),
                ],
                ..Default::default()
            },
            |ctx| out = ctx.input(|i| t.frame(i, 0.6)),
        );
        for s in &out.samples {
            assert!((s.pressure - 0.6).abs() < 1e-6);
        }
    }

    #[test]
    fn egui_modifiers_fold_the_platform_key_into_ctrl() {
        let m = modifiers_from_egui(egui::Modifiers {
            alt: true,
            ctrl: false,
            shift: true,
            mac_cmd: true,
            command: true,
        });
        assert!(m.alt && m.shift && m.ctrl);
        assert_eq!(
            modifiers_from_egui(egui::Modifiers::default()),
            tools::Modifiers::NONE
        );
    }
}
