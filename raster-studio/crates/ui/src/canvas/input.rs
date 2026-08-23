//! Input routing: from a pointer sample to either the active tool or the
//! camera, and never to both.
//!
//! # The rule
//!
//! A gesture is claimed when the pointer goes **down**, and the claim holds
//! until it comes up. Everything that decides *who* gets the gesture is decided
//! at that moment: whether the pointer was over the canvas at all, whether the
//! space bar was held, which button was pressed, which tool is active. After
//! that the events go where they were promised, even if the pointer leaves the
//! canvas — because a drag that stops working when the cursor crosses onto a
//! panel is worse than useless.
//!
//! The corollary is the one this module exists to guarantee: **a press that
//! starts over a panel reaches neither the tool nor the camera.** No pan, no
//! zoom, no stray brush dab under the layers list.
//!
//! # Pressure
//!
//! egui 0.29's input has no tablet pressure — its pointer events carry a
//! position and nothing else. [`PointerInput::pressure`] is therefore supplied
//! by the shell, which does have the winit/OS tablet stream, and defaults to
//! `1.0` (a mouse). This is a real seam, not a stub: the router clamps and
//! forwards whatever it is given, and [`crate::canvas::CanvasView`] exposes
//! [`crate::canvas::CanvasView::set_pen_pressure`] for the shell to feed it.

use glam::Vec2;
use tools::{Modifiers, PointerEvent, ToolId};

use super::camera::CanvasCamera;
use super::viewport::Viewport;

/// Which physical button a sample belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PointerButton {
    Primary,
    Secondary,
    Middle,
}

/// Where in a gesture a sample sits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PointerPhase {
    Down,
    Move,
    Up,
}

/// One raw pointer sample, in screen points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerInput {
    pub phase: PointerPhase,
    pub button: PointerButton,
    /// Position in screen points.
    pub pos_pt: Vec2,
    /// Stylus pressure in `0..=1`; `1.0` for a mouse.
    pub pressure: f32,
    pub modifiers: Modifiers,
}

impl PointerInput {
    /// A primary-button sample at full pressure with no modifiers.
    pub fn at(phase: PointerPhase, pos_pt: Vec2) -> Self {
        Self {
            phase,
            button: PointerButton::Primary,
            pos_pt,
            pressure: 1.0,
            modifiers: Modifiers::NONE,
        }
    }

    pub fn with_button(mut self, button: PointerButton) -> Self {
        self.button = button;
        self
    }

    pub fn with_pressure(mut self, pressure: f32) -> Self {
        self.pressure = pressure;
        self
    }

    pub fn with_modifiers(mut self, modifiers: Modifiers) -> Self {
        self.modifiers = modifiers;
        self
    }

    /// Pressure clamped into `0..=1`; a non-finite reading is a mouse.
    pub fn clamped_pressure(&self) -> f32 {
        if self.pressure.is_finite() {
            self.pressure.clamp(0.0, 1.0)
        } else {
            1.0
        }
    }
}

/// Who a gesture belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Route {
    /// The active tool, whichever it is.
    Tool(ToolId),
    /// Pan the view.
    Pan,
    /// Zoom the view.
    Zoom,
    /// Rotate the view.
    RotateView,
    /// The canvas's own chrome: a guide being pulled out of a ruler or dragged
    /// along. Claimed by [`InputRouter::claim`] rather than by
    /// [`InputRouter::route_for_press`], because whether a press lands on a
    /// guide is geometry the canvas knows and the router does not.
    Guide,
}

impl Route {
    /// `true` for the three routes the camera handles itself.
    pub const fn is_navigation(self) -> bool {
        matches!(self, Route::Pan | Route::Zoom | Route::RotateView)
    }
}

/// Why a sample was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rejected {
    /// The pointer was over a panel, and no gesture was in progress.
    OverPanel,
    /// The canvas has no area to receive the event.
    ViewportCollapsed,
    /// A button came up that never went down here, or a second button was
    /// pressed during someone else's gesture.
    NotOurGesture,
}

/// A sample that reached a tool, in document space.
#[derive(Debug, Clone, Copy)]
pub struct RoutedPointer {
    pub route: Route,
    pub phase: PointerPhase,
    pub button: PointerButton,
    /// The event as the tool wants it: document coordinates, pressure,
    /// modifiers.
    pub event: PointerEvent,
    /// The original screen position, for overlays that live in screen space.
    pub pos_pt: Vec2,
    /// `false` for a bare hover, with no button down.
    pub in_gesture: bool,
}

/// [`tools::PointerEvent`] has no `PartialEq`, so this is spelled out. Two
/// routed samples are equal when everything a caller can observe about them is.
impl PartialEq for RoutedPointer {
    fn eq(&self, other: &Self) -> bool {
        self.route == other.route
            && self.phase == other.phase
            && self.button == other.button
            && self.pos_pt == other.pos_pt
            && self.in_gesture == other.in_gesture
            && self.event.pos == other.event.pos
            && self.event.pressure == other.event.pressure
            && self.event.modifiers == other.event.modifiers
    }
}

/// What the router did with a sample.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Dispatch {
    /// Hand this to the active tool.
    ToTool(RoutedPointer),
    /// The camera consumed it; `changed` says whether the view actually moved.
    Navigated { route: Route, changed: bool },
    /// Nothing happened, and why.
    Rejected(Rejected),
}

impl Dispatch {
    /// The tool event, if there was one.
    pub fn tool_event(&self) -> Option<RoutedPointer> {
        match self {
            Dispatch::ToTool(r) => Some(*r),
            _ => None,
        }
    }

    pub fn is_rejected(&self) -> bool {
        matches!(self, Dispatch::Rejected(_))
    }
}

/// How many screen points of horizontal drag double the zoom on the zoom tool.
///
/// A gesture gain rather than a drawn size, but still expressed on the design
/// crate's grid — fifty units — so nothing in the canvas carries a bare screen
/// measurement and the two gains below stay in proportion to each other.
pub const ZOOM_DRAG_PT: f32 = design::UNIT_PT * 50.0;

/// How many points of wheel travel double the zoom. Thirty units: a wheel notch
/// is worth more than the same distance dragged.
pub const ZOOM_WHEEL_PT: f32 = design::UNIT_PT * 30.0;

/// What a scroll event should do.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum WheelAction {
    Zoom {
        anchor_pt: Vec2,
        factor: f32,
    },
    Pan {
        delta_pt: Vec2,
    },
    /// The pointer was not over the canvas.
    Ignored,
}

/// Per-gesture bookkeeping for the navigation routes.
#[derive(Debug, Clone, Copy, PartialEq)]
struct NavGesture {
    route: Route,
    button: PointerButton,
    /// Where the drag started, in screen points.
    start_pt: Vec2,
    /// The previous sample, in screen points.
    last_pt: Vec2,
    /// The document point grabbed at the start, for zoom and pan anchoring.
    anchor_doc: Vec2,
    /// The zoom when the gesture began.
    start_zoom: f32,
    /// The pointer's last angle about the content centre.
    last_angle: f32,
    /// Whether the pointer has actually moved, which is what tells a click from
    /// a drag.
    moved: bool,
}

/// Routes pointer samples, owns the temporary-hand state, and drives the
/// camera for the navigation gestures.
#[derive(Debug, Clone, Default)]
pub struct InputRouter {
    space_held: bool,
    active: Option<NavGesture>,
}

impl InputRouter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Tell the router whether the space bar is down.
    ///
    /// Takes effect on the *next* press: changing it mid-gesture must not
    /// hand a half-finished drag to somebody else, which is why the route is
    /// fixed at pointer-down.
    pub fn set_space_held(&mut self, held: bool) {
        self.space_held = held;
    }

    pub fn space_held(&self) -> bool {
        self.space_held
    }

    /// The route currently claiming the pointer, if any.
    pub fn active_route(&self) -> Option<Route> {
        self.active.map(|g| g.route)
    }

    /// `true` while a button is down and a route owns it.
    pub fn is_gesture_active(&self) -> bool {
        self.active.is_some()
    }

    /// Abandon the current gesture (Esc, focus loss). The camera keeps whatever
    /// it has already been moved to — a view change is not undoable, so there
    /// is nothing to roll back.
    pub fn cancel(&mut self) {
        self.active = None;
    }

    /// Claim the gesture for a route the router cannot work out for itself.
    ///
    /// The one caller is the guide drag in [`crate::canvas::CanvasView`]: a
    /// press on a ruler gutter or on a guide belongs to the canvas's own
    /// chrome, and claiming it here is what stops the same press also reaching
    /// the active tool. Refused — and reported as `false` — while another
    /// gesture is running, exactly like a second button during a drag.
    pub fn claim(&mut self, route: Route, at_pt: Vec2) -> bool {
        if self.active.is_some() || !at_pt.is_finite() {
            return false;
        }
        self.active = Some(NavGesture {
            route,
            button: PointerButton::Primary,
            start_pt: at_pt,
            last_pt: at_pt,
            anchor_doc: Vec2::ZERO,
            start_zoom: 1.0,
            last_angle: 0.0,
            moved: false,
        });
        true
    }

    /// Release a gesture claimed by [`InputRouter::claim`]. Reports whether the
    /// named route was the one holding the pointer, so a caller cannot release
    /// somebody else's gesture by mistake.
    pub fn release(&mut self, route: Route) -> bool {
        if self.active.map(|g| g.route) == Some(route) {
            self.active = None;
            true
        } else {
            false
        }
    }

    /// Which route a fresh press would claim.
    pub fn route_for_press(&self, input: &PointerInput, active_tool: ToolId) -> Route {
        if input.button == PointerButton::Middle || self.space_held {
            return Route::Pan;
        }
        match active_tool {
            ToolId::Hand => Route::Pan,
            ToolId::Zoom => Route::Zoom,
            ToolId::RotateView => Route::RotateView,
            other => Route::Tool(other),
        }
    }

    /// Route one sample.
    ///
    /// Navigation routes mutate `camera` in place and report
    /// [`Dispatch::Navigated`]; tool routes come back as [`Dispatch::ToTool`]
    /// in document coordinates for the caller to feed the active tool.
    pub fn handle(
        &mut self,
        input: PointerInput,
        camera: &mut CanvasCamera,
        viewport: &Viewport,
        active_tool: ToolId,
    ) -> Dispatch {
        if viewport.is_degenerate() {
            self.active = None;
            return Dispatch::Rejected(Rejected::ViewportCollapsed);
        }
        if !input.pos_pt.is_finite() {
            return Dispatch::Rejected(Rejected::NotOurGesture);
        }
        match input.phase {
            PointerPhase::Down => self.on_down(input, camera, viewport, active_tool),
            PointerPhase::Move => self.on_move(input, camera, viewport, active_tool),
            PointerPhase::Up => self.on_up(input, camera, viewport),
        }
    }

    fn on_down(
        &mut self,
        input: PointerInput,
        camera: &mut CanvasCamera,
        viewport: &Viewport,
        active_tool: ToolId,
    ) -> Dispatch {
        if self.active.is_some() {
            // A second button during someone else's gesture is ignored rather
            // than allowed to hijack it.
            return Dispatch::Rejected(Rejected::NotOurGesture);
        }
        if !viewport.contains_pt(input.pos_pt) {
            return Dispatch::Rejected(Rejected::OverPanel);
        }
        let route = self.route_for_press(&input, active_tool);
        let anchor_doc = camera.doc_of_screen_pt(viewport, input.pos_pt);
        self.active = Some(NavGesture {
            route,
            button: input.button,
            start_pt: input.pos_pt,
            last_pt: input.pos_pt,
            anchor_doc,
            start_zoom: camera.zoom,
            last_angle: angle_about(viewport.center_pt(), input.pos_pt),
            moved: false,
        });
        if route.is_navigation() {
            // Nothing moves on the press itself; a zoom click acts on release.
            Dispatch::Navigated {
                route,
                changed: false,
            }
        } else {
            Dispatch::ToTool(self.to_tool(route, input, camera, viewport, true))
        }
    }

    fn on_move(
        &mut self,
        input: PointerInput,
        camera: &mut CanvasCamera,
        viewport: &Viewport,
        active_tool: ToolId,
    ) -> Dispatch {
        let Some(mut gesture) = self.active else {
            // A bare hover. It reaches the tool — the brush cursor and the
            // path preview need it — but only over the canvas.
            if !viewport.contains_pt(input.pos_pt) {
                return Dispatch::Rejected(Rejected::OverPanel);
            }
            let route = Route::Tool(active_tool);
            return Dispatch::ToTool(self.to_tool(route, input, camera, viewport, false));
        };

        if (input.pos_pt - gesture.last_pt).length() > 0.0 {
            gesture.moved = true;
        }
        let route = gesture.route;
        let changed = match route {
            Route::Pan => {
                let delta = input.pos_pt - gesture.last_pt;
                camera.pan_screen_pt(viewport, delta);
                delta.length() > 0.0
            }
            Route::Zoom => {
                let dx = input.pos_pt.x - gesture.start_pt.x;
                let target = gesture.start_zoom * 2f32.powf(dx / ZOOM_DRAG_PT);
                let before = camera.zoom;
                let anchor = camera.screen_pt_of(viewport, gesture.anchor_doc);
                camera.set_zoom_about_screen_pt(viewport, anchor, target);
                (camera.zoom - before).abs() > f32::EPSILON
            }
            Route::RotateView => {
                let now = angle_about(viewport.center_pt(), input.pos_pt);
                let delta = shortest_angle(now - gesture.last_angle);
                gesture.last_angle = now;
                if delta != 0.0 && delta.is_finite() {
                    camera.rotate_by(delta);
                    true
                } else {
                    false
                }
            }
            Route::Tool(_) => {
                gesture.last_pt = input.pos_pt;
                self.active = Some(gesture);
                return Dispatch::ToTool(self.to_tool(route, input, camera, viewport, true));
            }
            // A guide drag is driven by the canvas, which consumes the sample
            // before the router ever sees it. Reaching here means the claim
            // outlived its driver, so the pointer is held and nothing moves.
            Route::Guide => false,
        };
        gesture.last_pt = input.pos_pt;
        self.active = Some(gesture);
        Dispatch::Navigated { route, changed }
    }

    fn on_up(
        &mut self,
        input: PointerInput,
        camera: &mut CanvasCamera,
        viewport: &Viewport,
    ) -> Dispatch {
        let Some(gesture) = self.active else {
            return Dispatch::Rejected(Rejected::NotOurGesture);
        };
        if gesture.button != input.button {
            return Dispatch::Rejected(Rejected::NotOurGesture);
        }
        self.active = None;
        let route = gesture.route;
        match route {
            Route::Tool(_) => Dispatch::ToTool(self.to_tool(route, input, camera, viewport, true)),
            Route::Zoom if !gesture.moved => {
                // A click, not a drag: step by a factor of two, out with alt.
                let anchor = camera.screen_pt_of(viewport, gesture.anchor_doc);
                if input.modifiers.alt {
                    camera.zoom_out(viewport, anchor);
                } else {
                    camera.zoom_in(viewport, anchor);
                }
                Dispatch::Navigated {
                    route,
                    changed: true,
                }
            }
            Route::RotateView if !gesture.moved && input.modifiers.alt => {
                // Alt-click puts the canvas back upright.
                let changed = camera.rotation != 0.0;
                camera.reset_rotation();
                Dispatch::Navigated { route, changed }
            }
            _ => Dispatch::Navigated {
                route,
                changed: false,
            },
        }
    }

    fn to_tool(
        &self,
        route: Route,
        input: PointerInput,
        camera: &CanvasCamera,
        viewport: &Viewport,
        in_gesture: bool,
    ) -> RoutedPointer {
        let doc = camera.doc_of_screen_pt(viewport, input.pos_pt);
        RoutedPointer {
            route,
            phase: input.phase,
            button: input.button,
            event: PointerEvent {
                pos: doc,
                pressure: input.clamped_pressure(),
                modifiers: input.modifiers,
            },
            pos_pt: input.pos_pt,
            in_gesture,
        }
    }

    /// What a scroll gesture should do. Never touches the camera itself, so a
    /// caller can gate it on focus.
    ///
    /// `delta_pt` is egui's scroll delta: positive y scrolls the content down.
    /// Ctrl zooms about the pointer; shift swaps the pan axes; otherwise the
    /// view pans.
    pub fn wheel(
        &self,
        delta_pt: Vec2,
        pos_pt: Vec2,
        modifiers: Modifiers,
        viewport: &Viewport,
    ) -> WheelAction {
        if viewport.is_degenerate() || !pos_pt.is_finite() || !delta_pt.is_finite() {
            return WheelAction::Ignored;
        }
        // A wheel over a panel belongs to that panel's scroll area.
        if !viewport.contains_pt(pos_pt) && !self.is_gesture_active() {
            return WheelAction::Ignored;
        }
        if modifiers.ctrl {
            let factor = 2f32.powf(delta_pt.y / ZOOM_WHEEL_PT);
            if !factor.is_finite() || factor <= 0.0 {
                return WheelAction::Ignored;
            }
            return WheelAction::Zoom {
                anchor_pt: pos_pt,
                factor,
            };
        }
        let delta = if modifiers.shift {
            Vec2::new(delta_pt.y, delta_pt.x)
        } else {
            delta_pt
        };
        WheelAction::Pan { delta_pt: delta }
    }

    /// Apply a [`WheelAction`] to the camera. Returns whether the view moved.
    pub fn apply_wheel(
        action: WheelAction,
        camera: &mut CanvasCamera,
        viewport: &Viewport,
    ) -> bool {
        match action {
            WheelAction::Ignored => false,
            WheelAction::Zoom { anchor_pt, factor } => {
                let before = camera.zoom;
                camera.zoom_about_screen_pt(viewport, anchor_pt, factor);
                (camera.zoom - before).abs() > f32::EPSILON
            }
            WheelAction::Pan { delta_pt } => {
                if delta_pt.length() <= 0.0 {
                    return false;
                }
                camera.pan_screen_pt(viewport, delta_pt);
                true
            }
        }
    }
}

fn angle_about(center: Vec2, p: Vec2) -> f32 {
    let d = p - center;
    if d.length_squared() <= f32::EPSILON {
        0.0
    } else {
        d.y.atan2(d.x)
    }
}

/// Fold an angle difference into `(-π, π]`, so crossing the branch cut does not
/// spin the canvas a full turn.
fn shortest_angle(d: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    if !d.is_finite() {
        return 0.0;
    }
    let mut x = d % TAU;
    if x > PI {
        x -= TAU;
    } else if x <= -PI {
        x += TAU;
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::viewport::PanelInsets;
    use std::f32::consts::FRAC_PI_2;

    fn vp() -> Viewport {
        Viewport::new(
            Vec2::new(1000.0, 800.0),
            PanelInsets::new(200.0, 100.0, 40.0, 20.0),
            2.0,
        )
    }

    fn cam() -> CanvasCamera {
        CanvasCamera {
            center: Vec2::new(200.0, 200.0),
            zoom: 2.0,
            ..CanvasCamera::default()
        }
    }

    fn inside(v: &Viewport) -> Vec2 {
        v.center_pt()
    }

    fn over_panel() -> Vec2 {
        // Inside the left dock.
        Vec2::new(50.0, 400.0)
    }

    fn down(p: Vec2) -> PointerInput {
        PointerInput::at(PointerPhase::Down, p)
    }
    fn moved(p: Vec2) -> PointerInput {
        PointerInput::at(PointerPhase::Move, p)
    }
    fn up(p: Vec2) -> PointerInput {
        PointerInput::at(PointerPhase::Up, p)
    }

    /// The headline guarantee.
    #[test]
    fn a_press_over_a_panel_reaches_neither_the_tool_nor_the_camera() {
        let v = vp();
        let mut c = cam();
        let before = c;
        let mut r = InputRouter::new();

        for tool in [
            ToolId::Brush,
            ToolId::Hand,
            ToolId::Zoom,
            ToolId::RotateView,
        ] {
            let d = r.handle(down(over_panel()), &mut c, &v, tool);
            assert_eq!(d, Dispatch::Rejected(Rejected::OverPanel), "{tool:?}");
            assert!(d.tool_event().is_none());
            assert_eq!(c, before, "{tool:?} moved the camera from a panel press");
            assert!(!r.is_gesture_active());

            // The drag that follows is equally ignored: no gesture was claimed.
            let m = r.handle(moved(inside(&v)), &mut c, &v, tool);
            assert!(m.tool_event().is_some_and(|e| !e.in_gesture) || m.is_rejected());
            assert_eq!(c, before, "{tool:?} panned on a move after a panel press");
        }
    }

    #[test]
    fn a_hover_over_a_panel_is_rejected_too() {
        let v = vp();
        let mut c = cam();
        let mut r = InputRouter::new();
        assert_eq!(
            r.handle(moved(over_panel()), &mut c, &v, ToolId::Brush),
            Dispatch::Rejected(Rejected::OverPanel)
        );
    }

    #[test]
    fn a_hover_over_the_canvas_reaches_the_tool_but_is_not_a_gesture() {
        let v = vp();
        let mut c = cam();
        let mut r = InputRouter::new();
        let e = r
            .handle(moved(inside(&v)), &mut c, &v, ToolId::Brush)
            .tool_event()
            .unwrap();
        assert!(!e.in_gesture);
        assert_eq!(e.route, Route::Tool(ToolId::Brush));
        assert!(!r.is_gesture_active());
    }

    #[test]
    fn a_tool_gesture_is_delivered_in_document_coordinates() {
        let v = vp();
        let mut c = cam();
        let mut r = InputRouter::new();
        let at = v.center_pt() + Vec2::new(20.0, -10.0);
        let e = r
            .handle(down(at), &mut c, &v, ToolId::Brush)
            .tool_event()
            .unwrap();
        assert_eq!(e.route, Route::Tool(ToolId::Brush));
        assert!(e.in_gesture);
        let want = c.doc_of_screen_pt(&v, at);
        assert!((e.event.pos - want).length() < 1e-4);
        assert_eq!(e.pos_pt, at);
    }

    /// Once a gesture is claimed it keeps running off the canvas, or a drag
    /// would die the moment the cursor crossed a panel.
    #[test]
    fn a_claimed_gesture_survives_the_pointer_leaving_the_canvas() {
        let v = vp();
        let mut c = cam();
        let mut r = InputRouter::new();
        r.handle(down(inside(&v)), &mut c, &v, ToolId::Brush);
        let e = r
            .handle(moved(over_panel()), &mut c, &v, ToolId::Brush)
            .tool_event()
            .unwrap();
        assert!(e.in_gesture);
        let u = r
            .handle(up(over_panel()), &mut c, &v, ToolId::Brush)
            .tool_event()
            .unwrap();
        assert_eq!(u.phase, PointerPhase::Up);
        assert!(!r.is_gesture_active());
    }

    #[test]
    fn the_hand_tool_pans_and_the_document_follows_the_drag() {
        let v = vp();
        let mut c = cam();
        let mut r = InputRouter::new();
        let start = inside(&v);
        let grabbed = c.doc_of_screen_pt(&v, start);
        r.handle(down(start), &mut c, &v, ToolId::Hand);
        let d = r.handle(
            moved(start + Vec2::new(40.0, 25.0)),
            &mut c,
            &v,
            ToolId::Hand,
        );
        assert_eq!(
            d,
            Dispatch::Navigated {
                route: Route::Pan,
                changed: true
            }
        );
        let now = c.screen_pt_of(&v, grabbed);
        assert!((now - (start + Vec2::new(40.0, 25.0))).length() < 1e-2);
    }

    #[test]
    fn the_space_bar_turns_any_tool_into_the_hand() {
        let v = vp();
        let mut c = cam();
        let mut r = InputRouter::new();
        r.set_space_held(true);
        assert!(r.space_held());
        let start = inside(&v);
        assert_eq!(
            r.handle(down(start), &mut c, &v, ToolId::Brush),
            Dispatch::Navigated {
                route: Route::Pan,
                changed: false
            }
        );
        assert_eq!(r.active_route(), Some(Route::Pan));
        let d = r.handle(
            moved(start + Vec2::new(10.0, 0.0)),
            &mut c,
            &v,
            ToolId::Brush,
        );
        assert!(matches!(
            d,
            Dispatch::Navigated {
                route: Route::Pan,
                ..
            }
        ));
        assert!(d.tool_event().is_none(), "the brush must not see the pan");
    }

    /// Releasing the space bar mid-drag must not hand the half-finished pan to
    /// the brush.
    #[test]
    fn releasing_space_mid_drag_does_not_hijack_the_gesture() {
        let v = vp();
        let mut c = cam();
        let mut r = InputRouter::new();
        r.set_space_held(true);
        let start = inside(&v);
        r.handle(down(start), &mut c, &v, ToolId::Brush);
        r.set_space_held(false);
        let d = r.handle(
            moved(start + Vec2::new(10.0, 0.0)),
            &mut c,
            &v,
            ToolId::Brush,
        );
        assert!(matches!(
            d,
            Dispatch::Navigated {
                route: Route::Pan,
                ..
            }
        ));
        assert!(d.tool_event().is_none());
        r.handle(up(start + Vec2::new(10.0, 0.0)), &mut c, &v, ToolId::Brush);
        // The next press goes to the brush, now that space is up.
        let e = r
            .handle(down(start), &mut c, &v, ToolId::Brush)
            .tool_event();
        assert!(e.is_some());
    }

    #[test]
    fn the_middle_button_pans_whatever_the_tool_is() {
        let v = vp();
        let mut c = cam();
        let mut r = InputRouter::new();
        let start = inside(&v);
        let d = r.handle(
            down(start).with_button(PointerButton::Middle),
            &mut c,
            &v,
            ToolId::Brush,
        );
        assert!(matches!(
            d,
            Dispatch::Navigated {
                route: Route::Pan,
                ..
            }
        ));
    }

    #[test]
    fn a_zoom_click_steps_in_and_alt_click_steps_out() {
        let v = vp();
        let mut c = cam();
        c.set_zoom(1.0);
        let mut r = InputRouter::new();
        let at = inside(&v);
        r.handle(down(at), &mut c, &v, ToolId::Zoom);
        r.handle(up(at), &mut c, &v, ToolId::Zoom);
        assert_eq!(c.zoom, 1.5, "a zoom click steps to the next rung");

        r.handle(down(at), &mut c, &v, ToolId::Zoom);
        r.handle(
            up(at).with_modifiers(Modifiers::alt()),
            &mut c,
            &v,
            ToolId::Zoom,
        );
        assert_eq!(c.zoom, 1.0);
    }

    #[test]
    fn dragging_the_zoom_tool_scales_smoothly_and_holds_its_anchor() {
        let v = vp();
        let mut c = cam();
        c.set_zoom(1.0);
        let mut r = InputRouter::new();
        let at = v.origin_pt() + Vec2::new(40.0, 60.0);
        let anchored = c.doc_of_screen_pt(&v, at);
        r.handle(down(at), &mut c, &v, ToolId::Zoom);
        r.handle(
            moved(at + Vec2::new(ZOOM_DRAG_PT, 0.0)),
            &mut c,
            &v,
            ToolId::Zoom,
        );
        assert!((c.zoom - 2.0).abs() < 1e-3, "{}", c.zoom);
        assert!((c.doc_of_screen_pt(&v, at) - anchored).length() < 0.5);
        // Dragging back the other way returns to where it started.
        r.handle(moved(at), &mut c, &v, ToolId::Zoom);
        assert!((c.zoom - 1.0).abs() < 1e-3);
        // …and having dragged, the release does not also step the zoom.
        r.handle(up(at), &mut c, &v, ToolId::Zoom);
        assert!((c.zoom - 1.0).abs() < 1e-3);
    }

    #[test]
    fn the_rotate_tool_spins_the_view_and_alt_click_resets_it() {
        let v = vp();
        let mut c = cam();
        let mut r = InputRouter::new();
        let centre = v.center_pt();
        let start = centre + Vec2::new(100.0, 0.0);
        r.handle(down(start), &mut c, &v, ToolId::RotateView);
        r.handle(
            moved(centre + Vec2::new(0.0, 100.0)),
            &mut c,
            &v,
            ToolId::RotateView,
        );
        assert!(
            (c.rotation - FRAC_PI_2).abs() < 1e-3,
            "rotation is {}",
            c.rotation
        );
        r.handle(
            up(centre + Vec2::new(0.0, 100.0)),
            &mut c,
            &v,
            ToolId::RotateView,
        );

        r.handle(
            down(centre + Vec2::new(5.0, 5.0)),
            &mut c,
            &v,
            ToolId::RotateView,
        );
        r.handle(
            up(centre + Vec2::new(5.0, 5.0)).with_modifiers(Modifiers::alt()),
            &mut c,
            &v,
            ToolId::RotateView,
        );
        assert_eq!(c.rotation, 0.0);
    }

    #[test]
    fn a_second_button_cannot_hijack_a_running_gesture() {
        let v = vp();
        let mut c = cam();
        let mut r = InputRouter::new();
        let at = inside(&v);
        r.handle(down(at), &mut c, &v, ToolId::Brush);
        let d = r.handle(
            down(at).with_button(PointerButton::Secondary),
            &mut c,
            &v,
            ToolId::Brush,
        );
        assert_eq!(d, Dispatch::Rejected(Rejected::NotOurGesture));
        assert_eq!(r.active_route(), Some(Route::Tool(ToolId::Brush)));
        // …and the wrong button coming up does not end it.
        let u = r.handle(
            up(at).with_button(PointerButton::Secondary),
            &mut c,
            &v,
            ToolId::Brush,
        );
        assert_eq!(u, Dispatch::Rejected(Rejected::NotOurGesture));
        assert!(r.is_gesture_active());
    }

    #[test]
    fn an_up_with_no_gesture_is_ignored() {
        let v = vp();
        let mut c = cam();
        let mut r = InputRouter::new();
        assert_eq!(
            r.handle(up(inside(&v)), &mut c, &v, ToolId::Brush),
            Dispatch::Rejected(Rejected::NotOurGesture)
        );
    }

    #[test]
    fn cancelling_frees_the_pointer_without_moving_the_view() {
        let v = vp();
        let mut c = cam();
        let mut r = InputRouter::new();
        r.handle(down(inside(&v)), &mut c, &v, ToolId::Hand);
        let after_press = c;
        r.cancel();
        assert!(!r.is_gesture_active());
        assert_eq!(c, after_press);
        // A move after cancelling is a plain hover.
        let e = r
            .handle(moved(inside(&v)), &mut c, &v, ToolId::Hand)
            .tool_event()
            .unwrap();
        assert!(!e.in_gesture);
    }

    #[test]
    fn a_collapsed_viewport_swallows_everything_and_drops_the_gesture() {
        let collapsed = Viewport::new(Vec2::splat(80.0), PanelInsets::uniform(80.0), 1.0);
        let mut c = cam();
        let before = c;
        let mut r = InputRouter::new();
        assert_eq!(
            r.handle(down(Vec2::splat(10.0)), &mut c, &collapsed, ToolId::Hand),
            Dispatch::Rejected(Rejected::ViewportCollapsed)
        );
        assert_eq!(c, before);
        assert!(!r.is_gesture_active());
    }

    #[test]
    fn pressure_and_modifiers_reach_the_tool_intact() {
        let v = vp();
        let mut c = cam();
        let mut r = InputRouter::new();
        let e = r
            .handle(
                down(inside(&v))
                    .with_pressure(0.42)
                    .with_modifiers(Modifiers::shift()),
                &mut c,
                &v,
                ToolId::Brush,
            )
            .tool_event()
            .unwrap();
        assert!((e.event.pressure - 0.42).abs() < 1e-6);
        assert!(e.event.modifiers.shift);
    }

    #[test]
    fn nonsense_pressure_becomes_a_mouse_and_out_of_range_is_clamped() {
        for (given, want) in [
            (f32::NAN, 1.0_f32),
            (f32::INFINITY, 1.0),
            (-3.0, 0.0),
            (5.0, 1.0),
            (0.5, 0.5),
        ] {
            let input = down(Vec2::ZERO).with_pressure(given);
            assert_eq!(input.clamped_pressure(), want, "{given}");
        }
    }

    #[test]
    fn a_nonsense_position_is_dropped_rather_than_poisoning_the_camera() {
        let v = vp();
        let mut c = cam();
        let before = c;
        let mut r = InputRouter::new();
        let d = r.handle(down(Vec2::new(f32::NAN, 0.0)), &mut c, &v, ToolId::Hand);
        assert!(d.is_rejected());
        assert_eq!(c, before);
    }

    #[test]
    fn the_wheel_pans_by_default_and_zooms_with_ctrl() {
        let v = vp();
        let r = InputRouter::new();
        let at = inside(&v);
        assert_eq!(
            r.wheel(Vec2::new(0.0, 30.0), at, Modifiers::NONE, &v),
            WheelAction::Pan {
                delta_pt: Vec2::new(0.0, 30.0)
            }
        );
        // Shift swaps the axes, which is how a horizontal scroll is expressed.
        assert_eq!(
            r.wheel(Vec2::new(0.0, 30.0), at, Modifiers::shift(), &v),
            WheelAction::Pan {
                delta_pt: Vec2::new(30.0, 0.0)
            }
        );
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
        match r.wheel(Vec2::new(0.0, ZOOM_WHEEL_PT), at, ctrl, &v) {
            WheelAction::Zoom { anchor_pt, factor } => {
                assert_eq!(anchor_pt, at);
                assert!((factor - 2.0).abs() < 1e-4);
            }
            other => panic!("expected a zoom, got {other:?}"),
        }
    }

    #[test]
    fn a_wheel_over_a_panel_does_not_move_the_canvas() {
        let v = vp();
        let r = InputRouter::new();
        assert_eq!(
            r.wheel(Vec2::new(0.0, 30.0), over_panel(), Modifiers::NONE, &v),
            WheelAction::Ignored
        );
        let mut c = cam();
        let before = c;
        assert!(!InputRouter::apply_wheel(WheelAction::Ignored, &mut c, &v));
        assert_eq!(c, before);
    }

    #[test]
    fn applying_a_wheel_zoom_keeps_the_pointer_anchored() {
        let v = vp();
        let mut c = cam();
        let r = InputRouter::new();
        let at = v.origin_pt() + Vec2::new(31.0, 17.0);
        let before = c.doc_of_screen_pt(&v, at);
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::NONE
        };
        let action = r.wheel(Vec2::new(0.0, 60.0), at, ctrl, &v);
        assert!(InputRouter::apply_wheel(action, &mut c, &v));
        assert!((c.doc_of_screen_pt(&v, at) - before).length() < 0.05);
    }

    #[test]
    fn the_route_for_a_press_is_a_pure_function_of_the_state() {
        let r = InputRouter::new();
        let input = down(Vec2::ZERO);
        assert_eq!(
            r.route_for_press(&input, ToolId::Brush),
            Route::Tool(ToolId::Brush)
        );
        assert_eq!(r.route_for_press(&input, ToolId::Hand), Route::Pan);
        assert_eq!(r.route_for_press(&input, ToolId::Zoom), Route::Zoom);
        assert_eq!(
            r.route_for_press(&input, ToolId::RotateView),
            Route::RotateView
        );
        assert!(Route::Pan.is_navigation());
        assert!(Route::Zoom.is_navigation());
        assert!(Route::RotateView.is_navigation());
        assert!(!Route::Tool(ToolId::Brush).is_navigation());
        // A guide drag is neither the tool's nor the camera's, and no press
        // resolves to it: it is claimed explicitly.
        assert!(!Route::Guide.is_navigation());
        for tool in ToolId::ALL {
            assert_ne!(r.route_for_press(&input, *tool), Route::Guide);
        }
    }

    /// A claimed guide gesture owns the pointer: the tool sees nothing, the
    /// camera does not move, and a second press cannot hijack it.
    #[test]
    fn a_claimed_guide_gesture_locks_out_the_tool_and_the_camera() {
        let v = vp();
        let mut c = cam();
        let before = c;
        let mut r = InputRouter::new();
        let at = inside(&v);

        assert!(r.claim(Route::Guide, at));
        assert_eq!(r.active_route(), Some(Route::Guide));
        assert!(r.is_gesture_active());
        // Claiming twice is refused rather than allowed to overwrite.
        assert!(!r.claim(Route::Guide, at));
        assert!(!r.claim(Route::Pan, at));

        // Anything that leaks through moves nothing and reaches nobody.
        let d = r.handle(down(at), &mut c, &v, ToolId::Brush);
        assert_eq!(d, Dispatch::Rejected(Rejected::NotOurGesture));
        let m = r.handle(moved(at + Vec2::new(20.0, 20.0)), &mut c, &v, ToolId::Brush);
        assert_eq!(
            m,
            Dispatch::Navigated {
                route: Route::Guide,
                changed: false
            }
        );
        assert!(m.tool_event().is_none());
        assert_eq!(c, before, "a guide drag moved the camera");

        // Only the route that claimed it can let it go.
        assert!(!r.release(Route::Pan));
        assert!(r.is_gesture_active());
        assert!(r.release(Route::Guide));
        assert!(!r.is_gesture_active());
        assert!(!r.release(Route::Guide));
        // …and now the brush works again.
        assert!(r
            .handle(down(at), &mut c, &v, ToolId::Brush)
            .tool_event()
            .is_some());
    }

    #[test]
    fn a_nonsense_claim_is_refused() {
        let mut r = InputRouter::new();
        assert!(!r.claim(Route::Guide, Vec2::new(f32::NAN, 0.0)));
        assert!(!r.is_gesture_active());
    }

    #[test]
    fn angles_fold_into_the_short_way_round() {
        use std::f32::consts::PI;
        assert!((shortest_angle(3.0 * PI) - PI).abs() < 1e-4);
        assert!((shortest_angle(-1.9 * PI) - 0.1 * PI).abs() < 1e-3);
        assert_eq!(shortest_angle(f32::NAN), 0.0);
        assert_eq!(angle_about(Vec2::ZERO, Vec2::ZERO), 0.0);
    }
}
