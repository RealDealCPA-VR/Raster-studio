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
//! * the pressure of each sample is [`pressure_of`] its force. winit 0.30 on
//!   Windows reports a pen's zero pressure as *no* force (its
//!   `normalize_pointer_pressure` maps `0` to `None`), so a forceless sample
//!   means two different things: from a **pen** it is zero pressure, from a
//!   **finger** (`WM_TOUCH`, which never carries a force) it is full pressure
//!   like the mouse. winit 0.30 exposes no pen/finger type on `Touch`, and on
//!   Windows every touch carries the same `device_id`, so the contact id is
//!   the key: an id that has ever reported a force, or that has hovered
//!   (sent `Moved` with nothing down; a finger cannot hover), is a pen, and
//!   a forceless sample from it is `0.0`, including the first sample of a
//!   contact that touches down at zero pressure (W8-A). An id never seen
//!   either way is a finger, and a forceless sample keeps the contact's
//!   reading: full for a contact that started without one. This is a
//!   heuristic, not a device type: Windows reuses contact ids, and pens and
//!   fingers share one pointer-id pool, so a finger that later gets an id a
//!   pen once used is read as that pen (a forceless finger then paints at
//!   zero pressure) until the id falls out of the bounded pen list;
//! * a pen hovering in range (a `Moved` with no contact down) becomes a
//!   hover sample ([`PenSample::hover`]), which the shell routes as the mouse
//!   routes a hover move and hands the brush ring as its position (W8-A);
//! * while a contact is down, mouse button and cursor events are the OS
//!   emulating that same contact, so they are swallowed — and a button that
//!   went down during a contact has its release swallowed too, even when the
//!   release arrives after the contact ended;
//! * losing window focus [`PenInput::reset`]s the state, since a contact
//!   whose lift was lost (winit on Windows never reports `Cancelled`) would
//!   otherwise swallow the mouse and every new contact for good. The contacts
//!   that were down are remembered as *lost*: their further `Moved` samples
//!   are dropped (not read as a pen hovering) until they end or start anew.
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
    /// The `0..=1` pressure to stamp on this sample. `1.0` on a hover.
    pub pressure: f32,
    /// A pen hovering in range with nothing down: a pointer move with no
    /// button held, never a stroke sample.
    pub hover: bool,
    /// The sample came from a contact id known to be a pen (it has reported
    /// a force, or hovered), rather than from a finger.
    pub pen: bool,
}

/// How many pen ids [`PenInput`] remembers. A tablet has one or two pens;
/// the bound only keeps a stream of fresh ids from growing the list.
const PEN_IDS: usize = 16;

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
    /// Contact ids known to be pens, oldest first (see the module docs).
    /// Kept across [`PenInput::reset`]: a lost lift does not make the pen a
    /// finger.
    pens: Vec<u64>,
    /// Contacts that went down while another was driving the pointer and
    /// are still down: their moves are neither strokes nor hovers.
    ignored: Vec<u64>,
    /// Contacts that were down (driving or ignored) when [`PenInput::reset`]
    /// forgot them. They may still be down, so their `Moved` is dropped —
    /// neither a stroke nor a pen hovering — until their `Ended`/`Cancelled`
    /// or a fresh `Started` for the id; without this a finger held through a
    /// focus loss would read as a hovering pen and be marked a pen for good.
    lost: Vec<u64>,
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
        let down = self.active.take().map(|c| c.id).into_iter();
        let down: Vec<u64> = down.chain(self.ignored.drain(..)).collect();
        for id in down {
            if !self.lost.contains(&id) {
                if self.lost.len() == PEN_IDS {
                    self.lost.remove(0);
                }
                self.lost.push(id);
            }
        }
        self.swallowed.clear();
    }

    /// Whether contact id `id` is known to be a pen.
    pub fn is_pen(&self, id: u64) -> bool {
        self.pens.contains(&id)
    }

    fn mark_pen(&mut self, id: u64) {
        if let Some(i) = self.pens.iter().position(|p| *p == id) {
            self.pens.remove(i);
        } else if self.pens.len() == PEN_IDS {
            self.pens.remove(0);
        }
        self.pens.push(id);
    }

    /// The pressure of one sample from contact `id`: its force when it has
    /// one; with none, `0.0` from a pen (winit's zero pressure) and `finger`
    /// otherwise.
    fn sample_pressure(&mut self, id: u64, force: Option<Force>, finger: f32) -> f32 {
        if force.is_some() {
            self.mark_pen(id);
            pressure_of(force)
        } else if self.is_pen(id) {
            0.0
        } else {
            finger
        }
    }

    /// Translate one winit touch event. `None` means "not ours to route": a
    /// second contact, a move/end for an id that is not the one down, or an
    /// end for an id that never started. A `Moved` with nothing down is a pen
    /// hovering in range (a finger cannot hover): it marks the id a pen and
    /// comes back as a [`PenSample::hover`] move.
    pub fn on_touch(
        &mut self,
        id: u64,
        phase: TouchPhase,
        pos: Vec2,
        force: Option<Force>,
    ) -> Option<PenSample> {
        match phase {
            TouchPhase::Started => {
                self.lost.retain(|i| *i != id);
                if self.active.is_some() {
                    if !self.ignored.contains(&id) {
                        self.ignored.push(id);
                    }
                    return None;
                }
                let pressure = self.sample_pressure(id, force, 1.0);
                self.active = Some(Contact { id, pressure });
                Some(PenSample {
                    phase: PointerPhase::Down,
                    pos,
                    pressure,
                    hover: false,
                    pen: self.is_pen(id),
                })
            }
            TouchPhase::Moved => {
                if self.ignored.contains(&id) || self.lost.contains(&id) {
                    return None;
                }
                let Some(contact) = self.active else {
                    self.mark_pen(id);
                    return Some(PenSample {
                        phase: PointerPhase::Move,
                        pos,
                        pressure: 1.0,
                        hover: true,
                        pen: true,
                    });
                };
                if contact.id != id {
                    return None;
                }
                let pressure = self.sample_pressure(id, force, contact.pressure);
                self.active = Some(Contact { id, pressure });
                Some(PenSample {
                    phase: PointerPhase::Move,
                    pos,
                    pressure,
                    hover: false,
                    pen: self.is_pen(id),
                })
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                self.ignored.retain(|i| *i != id);
                self.lost.retain(|i| *i != id);
                let contact = self.active.filter(|c| c.id == id)?;
                self.active = None;
                let pressure = self.sample_pressure(id, force, contact.pressure);
                Some(PenSample {
                    phase: PointerPhase::Up,
                    pos,
                    pressure,
                    hover: false,
                    pen: self.is_pen(id),
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
        let up = pen
            .on_touch(1, TouchPhase::Cancelled, p, Some(Force::Normalized(0.5)))
            .unwrap();
        assert_eq!((up.phase, up.pressure), (PointerPhase::Up, 0.5));
        assert!(!pen.is_active());
        // A hovering pen's move with nothing down is a hover, not a stroke.
        let hover = pen.on_touch(1, TouchPhase::Moved, p, None).unwrap();
        assert_eq!((hover.phase, hover.hover), (PointerPhase::Move, true));
        assert!(!pen.is_active());
    }

    #[test]
    fn a_pens_forceless_sample_is_zero_and_a_fingers_is_full() {
        let mut pen = PenInput::new();
        let p = Vec2::ZERO;
        // The pen reports forces on one contact...
        pen.on_touch(1, TouchPhase::Started, p, Some(Force::Normalized(0.6)));
        let mid = pen.on_touch(1, TouchPhase::Moved, p, None).unwrap();
        assert_eq!(mid.pressure, 0.0, "winit's zero pressure mid-contact");
        pen.on_touch(1, TouchPhase::Ended, p, None);
        // ...so its next contact touching down at zero pressure is zero.
        let down = pen.on_touch(1, TouchPhase::Started, p, None).unwrap();
        assert_eq!((down.pressure, down.pen), (0.0, true));
        let rise = pen
            .on_touch(1, TouchPhase::Moved, p, Some(Force::Normalized(0.4)))
            .unwrap();
        assert!((rise.pressure - 0.4).abs() < 1e-6);
        pen.on_touch(1, TouchPhase::Ended, p, None);
        // A finger (never a force, never hovered) is full pressure throughout.
        let finger = pen.on_touch(2, TouchPhase::Started, p, None).unwrap();
        assert_eq!((finger.pressure, finger.pen), (1.0, false));
        assert_eq!(
            pen.on_touch(2, TouchPhase::Moved, p, None)
                .unwrap()
                .pressure,
            1.0
        );
        pen.on_touch(2, TouchPhase::Ended, p, None);
        // An id first seen hovering is a pen before it ever reports a force.
        pen.on_touch(3, TouchPhase::Moved, p, None);
        assert_eq!(
            pen.on_touch(3, TouchPhase::Started, p, None)
                .unwrap()
                .pressure,
            0.0
        );
        pen.reset();
        assert!(
            pen.is_pen(1) && pen.is_pen(3),
            "a lost lift keeps the pen a pen"
        );
        assert!(!pen.is_pen(2));
        // A second finger left down after the first lifts is not a hover.
        pen.on_touch(4, TouchPhase::Started, p, None);
        pen.on_touch(5, TouchPhase::Started, p, None);
        pen.on_touch(4, TouchPhase::Ended, p, None);
        assert!(pen.on_touch(5, TouchPhase::Moved, p, None).is_none());
        pen.on_touch(5, TouchPhase::Ended, p, None);
        assert!(!pen.is_pen(5));
    }

    #[test]
    fn a_contact_held_through_a_reset_is_neither_a_hover_nor_a_pen() {
        let mut pen = PenInput::new();
        let p = Vec2::ZERO;
        // A finger (5) drives, a second finger (6) is ignored; focus is lost.
        pen.on_touch(5, TouchPhase::Started, p, None);
        pen.on_touch(6, TouchPhase::Started, p, None);
        pen.reset();
        assert!(!pen.is_active());
        // Both keep moving: dropped, not hovers, and never marked pens.
        assert!(pen.on_touch(5, TouchPhase::Moved, p, None).is_none());
        assert!(pen.on_touch(6, TouchPhase::Moved, p, None).is_none());
        assert!(!pen.is_pen(5) && !pen.is_pen(6));
        assert!(pen.on_touch(5, TouchPhase::Ended, p, None).is_none());
        assert!(pen.on_touch(6, TouchPhase::Cancelled, p, None).is_none());
        // A later finger reusing either id paints at full pressure.
        for id in [5, 6] {
            let down = pen.on_touch(id, TouchPhase::Started, p, None).unwrap();
            assert_eq!((down.pressure, down.pen), (1.0, false));
            let mv = pen.on_touch(id, TouchPhase::Moved, p, None).unwrap();
            assert_eq!((mv.pressure, mv.hover), (1.0, false));
            pen.on_touch(id, TouchPhase::Ended, p, None);
        }
        // An id no longer lost that moves with nothing down is a hover again.
        let hover = pen.on_touch(5, TouchPhase::Moved, p, None).unwrap();
        assert!(hover.hover);
    }

    #[test]
    fn a_pen_whose_lift_was_lost_does_not_hover_until_it_touches_down_again() {
        let mut pen = PenInput::new();
        let p = Vec2::ZERO;
        let force = Some(Force::Normalized(0.5));
        // A pen (9) is down when focus is lost, and its lift is never reported.
        pen.on_touch(9, TouchPhase::Started, p, force);
        pen.reset();
        // It lifts and hovers: every hover is dropped, however many arrive.
        for _ in 0..3 {
            assert!(pen.on_touch(9, TouchPhase::Moved, p, None).is_none());
        }
        // Its next touch-down paints, and once that lift is reported its
        // hovers move the pointer again.
        let down = pen.on_touch(9, TouchPhase::Started, p, force).unwrap();
        assert!(!down.hover && down.pen);
        pen.on_touch(9, TouchPhase::Ended, p, None);
        let hover = pen.on_touch(9, TouchPhase::Moved, p, None).unwrap();
        assert!(hover.hover);
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
