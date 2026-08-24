//! The Type tool: click to make a text layer, then type into it.
//!
//! # Why this exists
//!
//! `layer_model::TextLayer` and the whole `text-engine` were reachable from
//! nothing. The registry shipped no tool that could create a
//! [`LayerKind::Text`], so the only way a text layer ever appeared in a
//! document was by opening a `.psd` that already had one — there was no route
//! from a user gesture to a text layer at all.
//!
//! # The shape of a type gesture
//!
//! A click **creates the layer** and opens a [`TextSession`]. That is
//! deliberate and it is what makes the gesture undoable from its first moment:
//! the layer is a real [`Command::CreateLayer`] on the history the instant the
//! user clicks, so one Ctrl+Z takes the whole thing back. Every keystroke after
//! that is a [`Command::SetLayerKind`] carrying the *whole* run, which is the
//! only shape `editor-core` has for editing a layer's payload — and the same
//! path the Properties panel's text fields already take, so a shell that folds
//! a gesture's worth of them into one undo step
//! (`app_shell::Editor::apply_kind_edit`) folds these too.
//!
//! # What a session is not
//!
//! It is not a text editor. There is no selection, no word wrap, no click-to-
//! place-caret inside the run: the caret sits at the end of what has been
//! typed, [`TypeTool::insert`] appends and [`TypeTool::backspace`] removes one
//! grapheme-agnostic `char`. Placing the caret needs the glyph boxes
//! `ui::canvas::text_overlay` computes and a route for a click that lands
//! *inside* an existing text layer, and neither is wired. Stated rather than
//! implied.

use glam::{Affine2, Vec2};
use layer_model::{Layer, LayerId, LayerKind, TextLayer};

use editor_core::Command;

use crate::error::ToolError;
use crate::tool::{PointerEvent, TextEdit, Tool, ToolContext, ToolId};

/// The default face and size a fresh text layer is created with.
///
/// A family name rather than a font file: `text_engine` resolves families, and
/// a tool that named a path would be picking a font off the developer's
/// machine.
pub const DEFAULT_FONT_FAMILY: &str = "sans-serif";
pub const DEFAULT_SIZE_PX: f32 = 24.0;

/// The layer a type gesture is currently editing.
#[derive(Debug, Clone, PartialEq)]
pub struct TextSession {
    /// The layer the click created.
    pub layer: LayerId,
    /// Where it was clicked, in document pixels — the run's baseline origin,
    /// stored as the layer's transform.
    pub origin: Vec2,
    /// What has been typed so far.
    pub text: String,
}

/// Type: click to place a text layer, then type into it.
pub struct TypeTool {
    pub font_family: String,
    pub size_px: f32,
    /// Where the press landed, while the button is still down.
    pending: Option<Vec2>,
    session: Option<TextSession>,
}

impl Default for TypeTool {
    fn default() -> Self {
        Self {
            font_family: DEFAULT_FONT_FAMILY.to_string(),
            size_px: DEFAULT_SIZE_PX,
            pending: None,
            session: None,
        }
    }
}

impl TypeTool {
    /// The layer being edited, if any.
    pub fn session(&self) -> Option<&TextSession> {
        self.session.as_ref()
    }

    /// `true` while a layer is open for typing — what a shell reads to know
    /// that a character key belongs to the canvas rather than to the keymap.
    pub fn is_editing(&self) -> bool {
        self.session.is_some()
    }

    /// The payload of the session's layer as it stands.
    fn payload(&self, session: &TextSession) -> LayerKind {
        LayerKind::Text(TextLayer {
            text: session.text.clone(),
            font_family: self.font_family.clone(),
            size_px: self.size_px,
        })
    }

    /// Append text at the caret. Emits one [`Command::SetLayerKind`].
    ///
    /// Refused when nothing is being edited: a keystroke with no session is
    /// not a silent no-op, it is a shell routing a key to the wrong place.
    pub fn insert(&mut self, ctx: &mut ToolContext<'_>, text: &str) -> Result<(), ToolError> {
        let Some(session) = &mut self.session else {
            return Err(ToolError::NotStarted);
        };
        if text.is_empty() {
            return Ok(());
        }
        session.text.push_str(text);
        let session = session.clone();
        ctx.emit(Command::SetLayerKind {
            layer_id: session.layer,
            kind: Box::new(self.payload(&session)),
        });
        Ok(())
    }

    /// Remove the last character. Emits nothing when the run is already empty,
    /// so holding Backspace on an empty layer costs no history.
    pub fn backspace(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        let Some(session) = &mut self.session else {
            return Err(ToolError::NotStarted);
        };
        if session.text.pop().is_none() {
            return Ok(());
        }
        let session = session.clone();
        ctx.emit(Command::SetLayerKind {
            layer_id: session.layer,
            kind: Box::new(self.payload(&session)),
        });
        Ok(())
    }

    /// Leave the editing state, keeping what was typed. Reports the layer.
    ///
    /// Emits nothing: every keystroke has already been committed, so there is
    /// nothing left to write. What ends is only the claim on the keyboard.
    pub fn finish(&mut self) -> Option<LayerId> {
        self.session.take().map(|s| s.layer)
    }
}

impl Tool for TypeTool {
    fn id(&self) -> ToolId {
        ToolId::Type
    }

    /// The press only aims: it records where the layer would go and ends the
    /// run that was open, if any, so two clicks make two layers rather than one
    /// layer and one lost run.
    ///
    /// Nothing is emitted here. A gesture that is cancelled between the press
    /// and the release must leave nothing behind — the rule
    /// `every_tool_can_be_constructed_and_cancelled_without_panicking` keeps
    /// for every tool in the registry — and a layer created at the press would
    /// be a layer Escape could no longer take away.
    fn on_pointer_down(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("type origin", event.pos)?;
        self.finish();
        self.pending = Some(event.pos);
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        _event: PointerEvent,
    ) -> Result<(), ToolError> {
        // Dragging a text box out to a fixed width is paragraph text, which
        // `layer_model::TextLayer` has no field for. The release is what
        // counts, wherever the pointer wandered in between.
        Ok(())
    }

    /// The release makes the layer and opens it for typing.
    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        _event: PointerEvent,
    ) -> Result<(), ToolError> {
        let Some(origin) = self.pending.take() else {
            return Ok(());
        };
        let mut layer = Layer::with_kind(
            "Type",
            LayerKind::Text(TextLayer {
                text: String::new(),
                font_family: self.font_family.clone(),
                size_px: self.size_px,
            }),
        );
        // The click *is* the position: the run is authored at the layer's own
        // origin and the layer's transform is what puts it on the canvas, which
        // is the same convention every other transformed layer keeps.
        layer.transform = Affine2::from_translation(origin);
        let id = layer.id;
        ctx.emit(Command::create_layer(layer));
        self.session = Some(TextSession {
            layer: id,
            origin,
            text: String::new(),
        });
        Ok(())
    }

    /// Escape leaves the editing state and keeps the layer.
    ///
    /// The layer is not deleted, because [`Tool::cancel`] is contracted to emit
    /// nothing and deleting it would need a command. It is one Ctrl+Z away —
    /// the click that made it is a history step of its own.
    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.pending = None;
        self.finish();
    }

    /// Enter ends the run. It does **not** insert a newline: this build has one
    /// commit key for every tool that holds a gesture, and a text layer with no
    /// way to stop editing would swallow every shortcut afterwards.
    fn commit(&mut self, _ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        self.finish();
        Ok(())
    }

    fn has_pending_commit(&self) -> bool {
        self.session.is_some()
    }

    fn is_text_editing(&self) -> bool {
        self.session.is_some()
    }

    fn text_edit(
        &mut self,
        ctx: &mut ToolContext<'_>,
        edit: TextEdit<'_>,
    ) -> Result<(), ToolError> {
        match edit {
            TextEdit::Insert(text) => self.insert(ctx, text),
            TextEdit::Backspace => self.backspace(ctx),
        }
    }

    fn is_active(&self) -> bool {
        self.pending.is_some() || self.session.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiles::MemoryTiles;
    use raster::PixelRect;

    fn ctx(tiles: &mut MemoryTiles) -> ToolContext<'_> {
        ToolContext::new(tiles, PixelRect::new(0, 0, 128, 128))
    }

    fn click(tool: &mut TypeTool, ctx: &mut ToolContext<'_>, at: Vec2) {
        tool.on_pointer_down(ctx, PointerEvent::at(at.x, at.y))
            .unwrap();
        tool.on_pointer_up(ctx, PointerEvent::at(at.x, at.y))
            .unwrap();
    }

    #[test]
    fn a_click_creates_one_text_layer_at_the_clicked_point() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = TypeTool::default();
        click(&mut tool, &mut ctx, Vec2::new(30.0, 40.0));

        let commands = ctx.drain();
        assert_eq!(commands.len(), 1, "{commands:?}");
        let Command::CreateLayer { layer } = &commands[0] else {
            panic!("a click did not create a layer: {commands:?}");
        };
        assert!(matches!(layer.kind, LayerKind::Text(_)));
        assert_eq!(layer.transform.translation, Vec2::new(30.0, 40.0));
        assert!(layer.visible);
        assert_eq!(tool.session().map(|s| s.layer), Some(layer.id));
        assert!(tool.is_editing());
    }

    #[test]
    fn typing_rewrites_the_layers_run_and_backspace_takes_it_back() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = TypeTool::default();
        click(&mut tool, &mut ctx, Vec2::new(10.0, 10.0));
        let layer = tool.session().unwrap().layer;
        let _ = ctx.drain();

        tool.insert(&mut ctx, "Hi").unwrap();
        tool.insert(&mut ctx, "!").unwrap();
        assert_eq!(tool.session().unwrap().text, "Hi!");
        tool.backspace(&mut ctx).unwrap();
        assert_eq!(tool.session().unwrap().text, "Hi");

        let commands = ctx.drain();
        assert_eq!(commands.len(), 3, "one command per edit: {commands:?}");
        for command in &commands {
            let Command::SetLayerKind { layer_id, kind } = command else {
                panic!("a keystroke emitted {command:?}");
            };
            assert_eq!(*layer_id, layer);
            assert!(matches!(**kind, LayerKind::Text(_)));
        }
        let Command::SetLayerKind { kind, .. } = commands.last().unwrap() else {
            unreachable!()
        };
        let LayerKind::Text(run) = &**kind else {
            unreachable!()
        };
        assert_eq!(run.text, "Hi");
        assert_eq!(run.size_px, DEFAULT_SIZE_PX);
    }

    #[test]
    fn a_keystroke_with_no_session_is_refused_rather_than_ignored() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = TypeTool::default();
        assert!(tool.insert(&mut ctx, "x").is_err());
        assert!(tool.backspace(&mut ctx).is_err());
        assert!(ctx.commands().is_empty());
    }

    #[test]
    fn backspacing_an_empty_run_costs_no_history() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = TypeTool::default();
        click(&mut tool, &mut ctx, Vec2::ZERO);
        let _ = ctx.drain();
        tool.backspace(&mut ctx).unwrap();
        assert!(ctx.commands().is_empty(), "{:?}", ctx.commands());
    }

    #[test]
    fn a_second_click_makes_a_second_layer_rather_than_moving_the_first() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = TypeTool::default();
        click(&mut tool, &mut ctx, Vec2::new(5.0, 5.0));
        let first = tool.session().unwrap().layer;
        tool.insert(&mut ctx, "one").unwrap();
        click(&mut tool, &mut ctx, Vec2::new(60.0, 60.0));
        let second = tool.session().unwrap().layer;
        assert_ne!(first, second);
        assert_eq!(
            tool.session().unwrap().text,
            "",
            "the second layer inherited the first one's run"
        );
        let creates = ctx
            .commands()
            .iter()
            .filter(|c| matches!(c, Command::CreateLayer { .. }))
            .count();
        assert_eq!(creates, 2);
    }

    #[test]
    fn enter_and_escape_both_end_the_run_and_neither_deletes_the_layer() {
        for finish_with_enter in [true, false] {
            let mut tiles = MemoryTiles::new();
            let mut ctx = ctx(&mut tiles);
            let mut tool = TypeTool::default();
            click(&mut tool, &mut ctx, Vec2::new(1.0, 2.0));
            tool.insert(&mut ctx, "kept").unwrap();
            let _ = ctx.drain();

            assert!(tool.has_pending_commit());
            if finish_with_enter {
                Tool::commit(&mut tool, &mut ctx).unwrap();
            } else {
                Tool::cancel(&mut tool, &mut ctx);
            }
            assert!(!tool.is_editing());
            assert!(!tool.is_active());
            assert!(
                ctx.commands().is_empty(),
                "ending a run emitted {:?}",
                ctx.commands()
            );
        }
    }

    #[test]
    fn a_non_finite_click_is_refused_and_starts_nothing() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = TypeTool::default();
        assert!(tool
            .on_pointer_down(&mut ctx, PointerEvent::at(f32::NAN, 0.0))
            .is_err());
        assert!(!tool.is_editing());
        assert!(ctx.commands().is_empty());
    }
}
