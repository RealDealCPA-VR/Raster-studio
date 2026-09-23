//! W7-A: pen and touch input, translated onto the mouse's pointer route.
//!
//! winit 0.30 delivers a stylus (Windows `WM_POINTER`, and the touch stacks
//! on the other platforms) as [`winit::event::WindowEvent::Touch`], carrying
//! an optional [`Force`]. Nothing in the shell listened for it, so a tablet
//! either did nothing or reached the canvas only as the OS's emulated mouse,
//! which has no pressure — every stroke landed at full pressure.
//!
//! [`PenInput`] is the small state machine between that event and
//! `Shell::on_pointer`:
//!
//! * one contact at a time drives the pointer (the first id down); a second
//!   finger while it is down is ignored rather than teleporting the stroke;
//! * `Started` / `Moved` / `Ended` become `Down` / `Move` / `Up`, and a
//!   `Cancelled` contact is ended like `Ended`, so what was drawn is kept;
//! * the pressure of each sample is [`pressure_of`] its force; a contact that
//!   starts with no force (a finger) is full pressure like the mouse, and a
//!   sample that drops its force mid-contact (Windows reports a pen's zero
//!   pressure as "no force") keeps the contact's last reading instead of
//!   spiking to full;
//! * while a contact is down, mouse button and cursor events are the OS
//!   emulating that same contact, so they are swallowed — and a button that
//!   went down during a contact has its release swallowed too, even when the
//!   release arrives after the contact ended;
//! * losing window focus [`PenInput::reset`]s the state, since a contact
//!   whose lift was lost (winit on Windows never reports `Cancelled`) would
//!   otherwise swallow the mouse and every new contact for good.
//!
//! Verified with synthetic events only (see the shell's tests); it still
//! needs a physical pen on each platform to confirm the OS's event order.

use glam::Vec2;
use winit::event::{Force, MouseButton, TouchPhase};

use ui::canvas::PointerPhase;

/// The `0..=1` pressure a winit force reading stands for.
///
/// `Normalized` is used as is; `Calibrated` is `force / max_possible_force`
/// (the spec's device-independent scale, not winit's altitude-corrected
/// `normalized()`); no force — a finger, or a device that reports none — is
/// full pressure, exactly like the mouse. The result is clamped, and a
/// non-finite or degenerate reading is full pressure, so a bogus sample can
/// never veto a stroke.
pub fn pressure_of(force: Option<Force>) -> f32 {
    let raw = match force {
        None => return 1.0,
        Some(Force::Normalized(v)) => v,
        Some(Force::Calibrated {
            force,
            max_possible_force,
            ..
        }) if max_possible_force > 0.0 => force / max_possible_force,
        Some(Force::Calibrated { .. }) => return 1.0,
    };
    if raw.is_finite() {
        (raw as f32).clamp(0.0, 1.0)
    } else {
        1.0
    }
}

/// One pointer sample a touch event turned into.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PenSample {
    /// The pointer phase to route, exactly as a mouse button would.
    pub phase: PointerPhase,
    /// Window position in physical pixels — the same space `CursorMoved`
    /// reports, so it lands in the shell's `cursor` unchanged.
    pub pos: Vec2,
    /// The `0..=1` pressure to stamp on this sample.
    pub pressure: f32,
}

/// The contact currently driving the pointer.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Contact {
    id: u64,
    pressure: f32,
}

/// Touch/pen state between winit and the shell's pointer route.
#[derive(Debug, Default)]
pub struct PenInput {
    active: Option<Contact>,
    /// Mouse buttons pressed while a contact was down: their releases belong
    /// to the same emulated gesture and are swallowed too.
    swallowed: Vec<MouseButton>,
}

impl PenInput {
    /// A fresh state: no contact down, the mouse routes normally.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a touch/pen contact is currently driving the pointer.
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// Forget every contact and swallowed button, as if nothing were down.
    /// For a lost lift: the window losing focus mid-contact (Windows winit
    /// never reports a cancelled contact), after which the mouse and a new
    /// contact must route again.
    pub fn reset(&mut self) {
        self.active = None;
        self.swallowed.clear();
    }

    /// Translate one winit touch event. `None` means "not ours to route": a
    /// second contact, or a move/end for an id that never started (a pen
    /// hovering in range reports `Moved` without `Started`).
    pub fn on_touch(
        &mut self,
        id: u64,
        phase: TouchPhase,
        pos: Vec2,
        force: Option<Force>,
    ) -> Option<PenSample> {
        match phase {
            TouchPhase::Started => {
                if self.active.is_some() {
                    return None;
                }
                let pressure = pressure_of(force);
                self.active = Some(Contact { id, pressure });
                Some(PenSample {
                    phase: PointerPhase::Down,
                    pos,
                    pressure,
                })
            }
            TouchPhase::Moved => {
                let contact = self.active.as_mut().filter(|c| c.id == id)?;
                if force.is_some() {
                    contact.pressure = pressure_of(force);
                }
                Some(PenSample {
                    phase: PointerPhase::Move,
                    pos,
                    pressure: contact.pressure,
                })
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                let contact = self.active.filter(|c| c.id == id)?;
                self.active = None;
                let pressure = if force.is_some() {
                    pressure_of(force)
                } else {
                    contact.pressure
                };
                Some(PenSample {
                    phase: PointerPhase::Up,
                    pos,
                    pressure,
                })
            }
        }
    }

    /// Whether a mouse button event must be dropped as the OS's emulation of
    /// the active contact. A press during a contact is remembered so its
    /// release is dropped as well, whenever it arrives.
    pub fn swallow_mouse_button(&mut self, button: MouseButton, pressed: bool) -> bool {
        if pressed {
            if self.active.is_some() {
                if !self.swallowed.contains(&button) {
                    self.swallowed.push(button);
                }
                return true;
            }
            false
        } else if let Some(i) = self.swallowed.iter().position(|b| *b == button) {
            self.swallowed.remove(i);
            true
        } else {
            self.active.is_some()
        }
    }

    /// Whether a cursor move must be dropped: while a contact is down the
    /// contact's own samples position the pointer.
    pub fn swallow_cursor_move(&self) -> bool {
        self.active.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn force_maps_to_pressure() {
        assert_eq!(pressure_of(None), 1.0);
        assert_eq!(pressure_of(Some(Force::Normalized(0.2))), 0.2);
        assert_eq!(pressure_of(Some(Force::Normalized(3.0))), 1.0);
        assert_eq!(pressure_of(Some(Force::Normalized(f64::NAN))), 1.0);
        let calibrated = Force::Calibrated {
            force: 1.0,
            max_possible_force: 4.0,
            altitude_angle: None,
        };
        assert_eq!(pressure_of(Some(calibrated)), 0.25);
        let degenerate = Force::Calibrated {
            force: 1.0,
            max_possible_force: 0.0,
            altitude_angle: None,
        };
        assert_eq!(pressure_of(Some(degenerate)), 1.0);
    }

    #[test]
    fn one_contact_drives_and_a_second_is_ignored() {
        let mut pen = PenInput::new();
        let p = Vec2::new(1.0, 2.0);
        let down = pen.on_touch(1, TouchPhase::Started, p, Some(Force::Normalized(0.5)));
        assert_eq!(down.unwrap().phase, PointerPhase::Down);
        assert!(pen
            .on_touch(2, TouchPhase::Started, p, Some(Force::Normalized(1.0)))
            .is_none());
        assert!(pen.on_touch(2, TouchPhase::Moved, p, None).is_none());
        // A forceless mid-contact sample keeps the last reading.
        assert_eq!(
            pen.on_touch(1, TouchPhase::Moved, p, None)
                .unwrap()
                .pressure,
            0.5
        );
        let up = pen.on_touch(1, TouchPhase::Cancelled, p, None).unwrap();
        assert_eq!((up.phase, up.pressure), (PointerPhase::Up, 0.5));
        assert!(!pen.is_active());
        // A hovering pen's move with nothing down routes nothing.
        assert!(pen.on_touch(1, TouchPhase::Moved, p, None).is_none());
    }

    #[test]
    fn an_emulated_press_has_its_late_release_swallowed_too() {
        let mut pen = PenInput::new();
        assert!(!pen.swallow_mouse_button(MouseButton::Left, true));
        assert!(!pen.swallow_mouse_button(MouseButton::Left, false));
        pen.on_touch(7, TouchPhase::Started, Vec2::ZERO, None);
        assert!(pen.swallow_mouse_button(MouseButton::Left, true));
        assert!(pen.swallow_cursor_move());
        pen.on_touch(7, TouchPhase::Ended, Vec2::ZERO, None);
        assert!(!pen.swallow_cursor_move());
        // The release arrives after the contact ended: still the emulation's.
        assert!(pen.swallow_mouse_button(MouseButton::Left, false));
        // And the next real click routes.
        assert!(!pen.swallow_mouse_button(MouseButton::Left, true));
    }
}
