//! The `Tool` trait and its interaction context.

use glam::Vec2;

use editor_core::Command;
use layer_model::LayerId;

/// Stable identifier for a tool (for shortcut binding and persistence).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolId {
    Hand,
    Move,
    Crop,
    Brush,
    Eraser,
    RectSelect,
    EllipseSelect,
    Lasso,
    MagicWand,
    Clone,
    Gradient,
    Pen,
}

/// A pointer event in document (image-pixel) space.
#[derive(Debug, Clone, Copy)]
pub struct PointerEvent {
    /// Position in image-pixel coordinates.
    pub pos: Vec2,
    /// Stylus pressure 0..=1 (1.0 for mouse).
    pub pressure: f32,
    /// Modifier keys.
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

/// Everything a tool needs about the current editing target, plus an outbox for
/// commands it wants applied. The app drains `commands` and runs them through
/// history so undo/redo works uniformly.
pub struct ToolContext {
    /// The layer the tool operates on.
    pub active_layer: Option<LayerId>,
    /// Commands the tool wants applied this frame.
    pub commands: Vec<Command>,
}

impl ToolContext {
    pub fn new(active_layer: Option<LayerId>) -> Self {
        Self {
            active_layer,
            commands: Vec::new(),
        }
    }

    /// Queue a command to be applied through history.
    pub fn emit(&mut self, cmd: Command) {
        self.commands.push(cmd);
    }

    /// Take everything queued this interaction.
    pub fn drain(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.commands)
    }
}

/// The interface every interactive tool implements.
pub trait Tool {
    fn id(&self) -> ToolId;
    fn on_pointer_down(&mut self, ctx: &mut ToolContext, event: PointerEvent);
    fn on_pointer_move(&mut self, ctx: &mut ToolContext, event: PointerEvent);
    fn on_pointer_up(&mut self, ctx: &mut ToolContext, event: PointerEvent);
    /// Cancel an in-progress interaction (e.g. Esc); discard pending state.
    fn cancel(&mut self, ctx: &mut ToolContext);
}
