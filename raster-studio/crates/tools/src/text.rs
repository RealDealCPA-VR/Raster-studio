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
use layer_model::text::Frame;
use layer_model::{Layer, LayerId, LayerKind, TextLayer};

use editor_core::Command;

use crate::error::ToolError;
use crate::tool::{PointerEvent, TextEdit, Tool, ToolContext, ToolId, ToolRequest};

/// The default face and size a fresh text layer is created with.
///
/// A family name rather than a font file: `text_engine` resolves families, and
/// a tool that named a path would be picking a font off the developer's
/// machine.
pub const DEFAULT_FONT_FAMILY: &str = "sans-serif";
pub const DEFAULT_SIZE_PX: f32 = 24.0;
/// The registry's `size_px` range, which [`Tool::set_setting`] clamps into.
pub const MIN_SIZE_PX: f32 = 4.0;
pub const MAX_SIZE_PX: f32 = 512.0;

/// The family name the registry's `font_family` choice `index` names, read
/// from the registry's own spec so the list has exactly one home. An index
/// past the end clamps to the last entry, the options bar's own `conform`
/// rule; `None` only when the registry declares no such choice at all.
pub fn font_family_choice(index: usize) -> Option<&'static str> {
    let info = crate::registry::info(ToolId::Type)?;
    let spec = info.options.iter().find(|o| o.key == "font_family")?;
    let crate::registry::OptionKind::Choice { choices, .. } = spec.kind else {
        return None;
    };
    choices
        .get(index.min(choices.len().checked_sub(1)?))
        .copied()
}

/// Live IME preedit (card 025 stores the state; card 029 routes the events).
#[derive(Debug, Clone, PartialEq)]
pub struct Composition {
    /// The preedit string as typed so far.
    pub text: String,
    /// The byte range the preedit occupies inside the draft's text.
    pub range: std::ops::Range<usize>,
}

/// The layer a type gesture is currently editing (card 025: rich session
/// state, not an append-only string).
///
/// Nothing here touches the document's history: the draft rides the
/// [`ToolRequest::TextDraft`](crate::ToolRequest::TextDraft) outbox — applied
/// to the document directly so the canvas renders it live — and the session's
/// confirm/cancel reconciles: confirm emits the one `SetLayerKind` history
/// entry, cancel restores [`TextSession::original`] the same direct way (or
/// deletes a layer this session created).
#[derive(Debug, Clone, PartialEq)]
pub struct TextSession {
    /// The layer the session edits (created by this session, or an existing
    /// one entered — card 026).
    pub layer: LayerId,
    /// Where it was clicked, in document pixels — the run's baseline origin,
    /// stored as the layer's transform.
    pub origin: Vec2,
    /// The rich payload the layer had when the session began. Cancel restores
    /// it byte-for-byte, styles included.
    pub original: TextLayer,
    /// The draft being edited: what the layer becomes if the session commits.
    pub draft: TextLayer,
    /// Caret position: a byte index into [`TextSession::draft`]'s text, always
    /// on a char boundary.
    pub caret: usize,
    /// The selection's other end. `caret == anchor` is an empty selection;
    /// otherwise the range `min..max` is what edits replace.
    pub anchor: usize,
    /// Live IME preedit, if a composition is under way.
    pub composition: Option<Composition>,
    /// Whether this session created the layer (a click on empty canvas) rather
    /// than entered an existing one. Cancel deletes a created layer; an
    /// entered one is restored instead.
    pub created_layer: bool,
}

impl TextSession {
    /// The selected byte range, always ordered and inside the draft's text.
    #[must_use]
    pub fn selection(&self) -> std::ops::Range<usize> {
        self.anchor.min(self.caret)..self.anchor.max(self.caret)
    }

    /// Whether a non-empty range is selected.
    #[must_use]
    pub fn has_selection(&self) -> bool {
        self.anchor != self.caret
    }

    /// Move the caret to `index`, clamped into the draft's text and onto a
    /// char boundary — a caret inside a multi-byte character sits at that
    /// character's start (the engine's own boundary rule). Clears the
    /// selection: the anchor follows the caret.
    pub fn place_caret(&mut self, index: usize) {
        let index = clamp_boundary(&self.draft.text, index);
        self.caret = index;
        self.anchor = index;
    }

    /// Select the range between two byte indices (both clamped like
    /// [`Self::place_caret`]).
    pub fn select(&mut self, anchor: usize, caret: usize) {
        self.anchor = clamp_boundary(&self.draft.text, anchor);
        self.caret = clamp_boundary(&self.draft.text, caret);
    }

    /// Replace the selected range — or the empty range at the caret — with
    /// `text`, and leave the caret after the insertion with no selection.
    /// Returns the byte length inserted.
    ///
    /// Style spans shift minimally so the draft always validates: a span
    /// entirely before the edit keeps its bounds, one entirely after shifts
    /// whole, and one the edit lands inside keeps its start and takes the
    /// size change at its end. Card 028 owns the richer style-carrying rules
    /// (what style inserted text receives); this is the validity floor.
    pub fn replace_selection(&mut self, text: &str) -> usize {
        let range = self.selection();
        let mut draft = std::mem::take(&mut self.draft);
        let mut next = String::with_capacity(draft.text.len() + text.len());
        next.push_str(&draft.text[..range.start]);
        let inserted = text.len();
        next.push_str(text);
        next.push_str(&draft.text[range.end..]);
        shift_spans(
            &mut draft.spans,
            range.start,
            range.end - range.start,
            inserted,
            next.len(),
        );
        draft.text = next;
        self.draft = draft;
        self.caret = range.start + inserted;
        self.anchor = self.caret;
        self.composition = None;
        inserted
    }

    /// Delete the selected range (or the empty range at the caret, which is a
    /// no-op). Returns whether anything was removed.
    pub fn delete_selection(&mut self) -> bool {
        if !self.has_selection() {
            return false;
        }
        self.replace_selection("");
        true
    }

    // -- card 027: caret movement and range selection ----------------------
    //
    // All movement is in bytes but only ever lands on char boundaries — UTF-8
    // is never split, and a selection is always a valid half-open byte range
    // of the draft. With `extend` the anchor stays (Shift-selection); without
    // it the anchor follows the caret (the selection collapses). Visual
    // (shaped) geometry for bidi refinement rides the overlay route (card
    // 030); these primitives are the session's own truth.

    /// Move the caret `delta` characters (negative = towards the start),
    /// stepping over multi-byte characters whole. Stops at the text's ends.
    pub fn move_horizontally(&mut self, delta: isize, extend: bool) {
        let text = self.draft.text.clone();
        let step = delta.signum();
        for _ in 0..delta.unsigned_abs() {
            if step > 0 && self.caret < text.len() {
                let mut next = self.caret + 1;
                while next < text.len() && !text.is_char_boundary(next) {
                    next += 1;
                }
                self.caret = next;
            } else if step < 0 && self.caret > 0 {
                self.caret = clamp_boundary(&text, self.caret - 1);
            }
        }
        if !extend {
            self.anchor = self.caret;
        }
    }

    /// Move to the current paragraph's start (`end == false`) or end — a
    /// paragraph break is a newline; Home/End never leave the paragraph.
    pub fn move_to_paragraph_edge(&mut self, end: bool, extend: bool) {
        let text = &self.draft.text;
        let caret = self.caret.min(text.len());
        let newline = char::from(10); // the paragraph break
        let target = if end {
            text[caret..]
                .find(newline)
                .map_or(text.len(), |off| caret + off)
        } else {
            text[..caret].rfind(newline).map_or(0, |off| off + 1)
        };
        self.caret = target;
        if !extend {
            self.anchor = self.caret;
        }
    }

    /// Move one word. Forward: to the end of the current word run (or the
    /// next one, when sitting between words), skipping trailing spaces.
    /// Backward: to the start of the preceding word run. Word characters are
    /// alphanumeric, `_`, and anything non-ASCII (so accented words and CJK
    /// move as words too); multi-byte characters step whole.
    pub fn move_by_word(&mut self, forward: bool, extend: bool) {
        let text = self.draft.text.clone();
        let bytes = text.as_bytes();
        let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80;
        let mut i = self.caret;
        if forward {
            if i < text.len() && is_word(bytes[i]) {
                while i < text.len() && is_word(bytes[i]) {
                    i += 1;
                }
                while i < text.len() && (bytes[i] as char).is_whitespace() {
                    i += 1;
                }
            } else {
                while i < text.len() && !is_word(bytes[i]) {
                    i += 1;
                }
            }
        } else {
            while i > 0 && (bytes[i - 1] as char).is_whitespace() {
                i -= 1;
            }
            if i > 0 && is_word(bytes[i - 1]) {
                while i > 0 && is_word(bytes[i - 1]) {
                    i = clamp_boundary(&text, i - 1);
                }
            } else {
                while i > 0 && !is_word(bytes[i - 1]) {
                    i = clamp_boundary(&text, i - 1);
                }
            }
        }
        self.caret = i;
        if !extend {
            self.anchor = self.caret;
        }
    }

    /// Delete the character after the caret (or the selected range) — the
    /// forward Delete key. Returns whether anything was removed.
    pub fn delete_forward(&mut self) -> bool {
        if self.has_selection() {
            return self.delete_selection();
        }
        if self.caret >= self.draft.text.len() {
            return false;
        }
        let next = {
            let text = self.draft.text.clone();
            let mut next = self.caret + 1;
            while next < text.len() && !text.is_char_boundary(next) {
                next += 1;
            }
            next
        };
        self.anchor = next;
        self.delete_selection()
    }

    /// Delete the character before the caret (or the selected range) — the
    /// Backspace key's primitive. Returns whether anything was removed.
    pub fn delete_backward(&mut self) -> bool {
        if self.has_selection() {
            return self.delete_selection();
        }
        if self.caret == 0 {
            return false;
        }
        self.caret = clamp_boundary(&self.draft.text, self.caret - 1);
        self.delete_selection()
    }

    /// Delete one word: backward deletes the run before the caret (the
    /// backward word-step's reach), forward deletes the forward word-step's
    /// reach. Returns whether anything was removed.
    pub fn delete_word(&mut self, forward: bool) -> bool {
        if self.has_selection() {
            return self.delete_selection();
        }
        let before = self.caret;
        self.move_by_word(forward, false);
        let (start, end) = if forward {
            (before, self.caret)
        } else {
            (self.caret, before)
        };
        if start == end {
            return false;
        }
        self.select(start, end);
        self.delete_selection()
    }

    /// The selected text, if any — the OS clipboard copy source (card 028).
    pub fn selected_text(&self) -> Option<String> {
        let start = self.selection().start;
        let end = self.selection().end;
        if start >= end {
            return None;
        }
        Some(self.draft.text[start..end].to_owned())
    }

    /// Select the whole draft: anchor at the start, caret at the end.
    pub fn select_all(&mut self) {
        self.anchor = 0;
        self.caret = self.draft.text.len();
    }

    /// Begin an IME composition: the preedit sits at the caret (replacing any
    /// selection AND any previous preedit — the composition is replaced
    /// wholesale, never accumulated) and the caret rests after it.
    pub fn set_composition(&mut self, text: &str) {
        if let Some(existing) = self.composition.take() {
            // Remove the previous preedit: the selection is exactly where it
            // sat, so the new preedit replaces it byte-for-byte.
            self.caret = existing.range.start;
            self.anchor = existing.range.end;
        }
        self.replace_selection(text);
        let end = self.caret;
        self.composition = Some(Composition {
            text: text.to_string(),
            range: end - text.len()..end,
        });
    }

    /// Commit the live composition (or a no-op one): the preedit becomes
    /// ordinary draft text and the caret rests after it.
    pub fn commit_composition(&mut self) {
        if let Some(existing) = self.composition.take() {
            self.caret = existing.range.end;
            self.anchor = self.caret;
        }
    }

    /// Abandon the live composition: the preedit text is removed and the
    /// caret returns to where the composition began.
    pub fn clear_composition(&mut self) {
        if let Some(existing) = self.composition.take() {
            self.caret = existing.range.start;
            self.anchor = existing.range.end;
            self.replace_selection("");
        }
    }
}

/// Clamp a byte index into `text` and back onto a char boundary.
fn clamp_boundary(text: &str, index: usize) -> usize {
    let index = index.min(text.len());
    let mut index = index;
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Keep style spans inside the text after an edit at `at` that removed
/// `removed` bytes and inserted `inserted` (the validity floor; card 028
/// owns the style-carrying rules).
fn shift_spans(
    spans: &mut [layer_model::text::StyleSpan],
    at: usize,
    removed: usize,
    inserted: usize,
    new_len: usize,
) {
    let delta = inserted as isize - removed as isize;
    let shift = |v: usize| -> usize { (v as isize + delta).max(0) as usize };
    for span in spans.iter_mut() {
        if span.end <= at {
            continue;
        }
        if span.start >= at + removed {
            span.start = shift(span.start);
            span.end = shift(span.end);
        } else {
            // The edit lands inside or at the edge of this span: a start
            // inside the removed range collapses to the edit point, and the
            // span takes the size change at its end.
            span.start = span.start.min(at);
            span.end = shift(span.end).max(span.start);
        }
        // Everything clamps into the new text — a span past its own text is
        // exactly the corruption `TextLayer::validate` refuses.
        span.start = span.start.min(new_len);
        span.end = span.end.max(span.start).min(new_len);
    }
}

/// Type: click to place a text layer, then type into it.
/// Card 032: a Type drag wider than this (in layer pixels) creates a
/// paragraph box instead of point text.
const PARAGRAPH_DRAG_THRESHOLD_PX: f32 = 6.0;

/// Card 032: the narrowest box a drag may produce (the panel's own minimum).
const PARAGRAPH_MIN_BOX_WIDTH_PX: f32 = 8.0;

/// Card 032: the widest box the session route emits — mirroring the
/// Paragraph panel's cap, so a huge value cannot publish garbage overlay
/// geometry or overflow the screen mapping.
const PARAGRAPH_MAX_BOX_WIDTH_PX: f32 = 4096.0;

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

    /// The payload of the session's layer as it stands: the draft, verbatim.
    fn payload(session: &TextSession) -> LayerKind {
        LayerKind::Text(session.draft.clone())
    }

    /// Insert text at the caret, replacing any selected range.
    ///
    /// Emits no history command: the draft rides the
    /// [`ToolRequest::TextDraft`](crate::ToolRequest::TextDraft) outbox so the
    /// canvas renders it live while history stays clean — the session's
    /// confirm is the one history entry (card 025).
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
        session.replace_selection(text);
        let session = session.clone();
        ctx.emit_request(ToolRequest::TextDraft {
            layer: session.layer,
            kind: Box::new(Self::payload(&session)),
        });
        Ok(())
    }

    /// Remove the character before the caret, or the selected range.
    ///
    /// Emits nothing when the draft is empty and nothing is selected, so
    /// holding Backspace on an empty layer costs no redraw.
    pub fn backspace(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        let Some(session) = &mut self.session else {
            return Err(ToolError::NotStarted);
        };
        if session.has_selection() {
            if !session.delete_selection() {
                return Ok(());
            }
        } else {
            // One character back: the caret steps over multi-byte characters
            // whole, never into the middle of one. The anchor stays at the
            // old caret, so the selection is exactly that character.
            if session.caret == 0 {
                return Ok(());
            }
            session.caret = clamp_boundary(&session.draft.text, session.caret - 1);
            if !session.delete_selection() {
                return Ok(());
            }
        }
        let session = session.clone();
        ctx.emit_request(ToolRequest::TextDraft {
            layer: session.layer,
            kind: Box::new(Self::payload(&session)),
        });
        Ok(())
    }

    /// End the session, committing the draft as ONE history entry.
    ///
    /// The draft has been living on the document outside history, so the
    /// [`ToolRequest::TextConfirm`] the shell performs restores the original
    /// payload first and only then applies the `SetLayerKind` — the command's
    /// inverse is captured from the original, so one Ctrl+Z after a confirmed
    /// run takes the whole session back at once (card 025).
    pub fn confirm(&mut self, ctx: &mut ToolContext<'_>) -> Option<LayerId> {
        let session = self.session.take()?;
        ctx.emit_request(ToolRequest::TextConfirm {
            layer: session.layer,
            original: Box::new(LayerKind::Text(session.original.clone())),
            draft: Box::new(Self::payload(&session)),
        });
        Some(session.layer)
    }

    /// Leave the editing state without committing.
    ///
    /// A layer this session created is deleted (a cancelled click must not
    /// leave a stray layer); a layer it entered is restored to its original
    /// payload — styles included — through the same history-free draft route,
    /// so cancel leaves no history entry at all.
    pub fn cancel_session(&mut self, ctx: &mut ToolContext<'_>) -> Option<LayerId> {
        let session = self.session.take()?;
        if session.created_layer {
            ctx.emit(Command::DeleteLayer {
                layer_id: session.layer,
            });
        } else {
            ctx.emit_request(ToolRequest::TextDraft {
                layer: session.layer,
                kind: Box::new(LayerKind::Text(session.original.clone())),
            });
        }
        Some(session.layer)
    }

    /// Leave the editing state, keeping what was typed. Reports the layer.
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
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        crate::error::finite_pt("type origin", event.pos)?;
        // Card 025: the outgoing session is CONFIRMED, not silently dropped —
        // its typed draft is already on the layer history-free, so the
        // TextConfirm request is what lands it as one history entry before
        // the new layer is created. (The old finish() here stranded that
        // draft: visible, but recorded by nothing.)
        self.confirm(ctx);
        self.pending = Some(event.pos);
        Ok(())
    }

    fn on_pointer_move(
        &mut self,
        _ctx: &mut ToolContext<'_>,
        _event: PointerEvent,
    ) -> Result<(), ToolError> {
        // Card 032: the release measures the drag against the down point, so
        // the move itself needs no state — wherever the pointer wandered,
        // the up event's position is what sizes the box.
        Ok(())
    }

    /// The release makes the layer and opens it for typing — or, when the
    /// shell resolved a text layer under the click (card 026), ENTERS that
    /// layer: no new layer, the caret where the shaped hit test said.
    fn on_pointer_up(
        &mut self,
        ctx: &mut ToolContext<'_>,
        event: PointerEvent,
    ) -> Result<(), ToolError> {
        let Some(origin) = self.pending.take() else {
            return Ok(());
        };
        // Card 032: a drag wider than the threshold creates paragraph text —
        // a wrapping box whose width is the drag's horizontal extent; a click
        // keeps creating point text. Either way the session opens the same.
        let drag_width = (event.pos - origin).abs().x;
        if let Some(mut hit) = ctx.text_hit.take() {
            // Belt and braces: the facade's caret comes from caret stops
            // (cluster boundaries), but card 027 layers movement on top — the
            // session never accepts a mid-character caret.
            hit.caret = clamp_boundary(&hit.original.text, hit.caret);
            self.session = Some(TextSession {
                layer: hit.layer,
                origin,
                original: hit.original.clone(),
                draft: hit.original,
                caret: hit.caret,
                anchor: hit.caret,
                composition: None,
                created_layer: false,
            });
            return Ok(());
        }
        let mut layer = Layer::with_kind(
            "Type",
            LayerKind::Text(TextLayer {
                text: String::new(),
                font_family: self.font_family.clone(),
                size_px: self.size_px,
                frame: if drag_width > PARAGRAPH_DRAG_THRESHOLD_PX {
                    Frame::Box {
                        width: drag_width.max(PARAGRAPH_MIN_BOX_WIDTH_PX),
                        height: None,
                    }
                } else {
                    Frame::Point
                },
                ..Default::default()
            }),
        );
        // The click *is* the position: the run is authored at the layer's own
        // origin and the layer's transform is what puts it on the canvas, which
        // is the same convention every other transformed layer keeps.
        layer.transform = Affine2::from_translation(origin);
        let id = layer.id;
        let original = match &layer.kind {
            LayerKind::Text(text) => text.clone(),
            _ => unreachable!("the layer was just built as text"),
        };
        ctx.emit(Command::create_layer(layer));
        self.session = Some(TextSession {
            layer: id,
            origin,
            // Card 032: the draft IS the created payload — box frame and all,
            // not a fresh default that would drop the drag-sized box.
            original: original.clone(),
            draft: original,
            caret: 0,
            anchor: 0,
            composition: None,
            created_layer: true,
        });
        Ok(())
    }

    /// Escape leaves the editing state and keeps the layer.
    ///
    /// [`Tool::cancel`] is contracted to emit nothing, so the rich cancel
    /// semantics (restore or delete, card 025) ride the shell's
    /// `TextEdit::Cancel` route instead — this only handles the contract case
    /// where the tool is dropped mid-gesture without a drained outbox.
    fn cancel(&mut self, _ctx: &mut ToolContext<'_>) {
        self.pending = None;
        self.finish();
    }

    /// Enter ends the run as a confirm (card 025): the draft commits as one
    /// history entry. It does **not** insert a newline: this build has one
    /// commit key for every tool that holds a gesture, and a text layer with
    /// no way to stop editing would swallow every shortcut afterwards.
    fn commit(&mut self, ctx: &mut ToolContext<'_>) -> Result<(), ToolError> {
        self.confirm(ctx);
        Ok(())
    }

    fn has_pending_commit(&self) -> bool {
        self.session.is_some()
    }

    /// The two options the registry declares for Type reach the tool and,
    /// through [`Self::on_pointer_up`], the text layer the next click
    /// creates: `size_px` (a Float, the registry's 4..512 range) and
    /// `font_family` (a Choice indexing the registry's family list, so the
    /// tool and the options bar cannot disagree about which name index 1
    /// is). A live session keeps the payload it opened with — the draft is
    /// what the user is typing into, and re-sizing it from under them rides
    /// the shell's text-edit route, not an options seed.
    ///
    /// Before this, `size_px` was refused and `font_family` was accepted by
    /// the trait default and dropped: every layer came out 24px sans.
    fn set_setting(
        &mut self,
        key: &str,
        setting: crate::tool::ToolSetting,
    ) -> Result<(), ToolError> {
        use crate::tool::ToolSetting;
        match (key, setting) {
            ("size_px", ToolSetting::Float(v)) => {
                crate::error::finite("type size", v)?;
                self.size_px = v.clamp(MIN_SIZE_PX, MAX_SIZE_PX);
                Ok(())
            }
            ("font_family", ToolSetting::Choice(index)) => {
                let Some(family) = font_family_choice(index) else {
                    return Err(ToolError::UnknownOption {
                        key: key.to_owned(),
                    });
                };
                self.font_family = family.to_owned();
                Ok(())
            }
            ("size_px", _) | ("font_family", _) => Err(ToolError::OptionKindMismatch {
                key: key.to_owned(),
            }),
            _ => Err(ToolError::UnknownOption {
                key: key.to_owned(),
            }),
        }
    }

    fn is_text_editing(&self) -> bool {
        self.session.is_some()
    }

    fn text_session_layer(&self) -> Option<LayerId> {
        self.session.as_ref().map(|s| s.layer)
    }

    fn text_selection_text(&self) -> Option<String> {
        self.session.as_ref().and_then(TextSession::selected_text)
    }

    fn text_composing(&self) -> bool {
        self.session
            .as_ref()
            .is_some_and(|s| s.composition.is_some())
    }

    fn text_caret_anchor(&self) -> Option<(usize, usize)> {
        let session = self.session.as_ref()?;
        Some((session.caret, session.anchor))
    }

    fn enter_text_session(
        &mut self,
        layer: LayerId,
        original: TextLayer,
        caret: usize,
        origin: Vec2,
    ) -> bool {
        if self.session.is_some() {
            return false;
        }
        self.session = Some(TextSession {
            layer,
            origin,
            original: original.clone(),
            draft: original,
            caret,
            anchor: caret,
            composition: None,
            created_layer: false,
        });
        true
    }

    fn text_edit(
        &mut self,
        ctx: &mut ToolContext<'_>,
        edit: TextEdit<'_>,
    ) -> Result<(), ToolError> {
        match edit {
            TextEdit::Insert(text) => self.insert(ctx, text),
            TextEdit::Backspace => self.backspace(ctx),
            TextEdit::Confirm => {
                self.confirm(ctx);
                Ok(())
            }
            TextEdit::Cancel => {
                self.cancel_session(ctx);
                Ok(())
            }
            // Card 027: movement never changes the text — nothing to render,
            // nothing to commit.
            // Card 027 note folded into 029: a live preedit's range goes
            // stale the moment the caret moves, so any caret-moving edit
            // commits it first — the typed text stays as ordinary draft text
            // (no data loss) and the IME's next preedit starts fresh.
            TextEdit::CaretStep { back, extend } => {
                if let Some(session) = &mut self.session {
                    session.commit_composition();
                    session.move_horizontally(if back { -1 } else { 1 }, extend);
                }
                Ok(())
            }
            TextEdit::ParagraphEdge { end, extend } => {
                if let Some(session) = &mut self.session {
                    session.commit_composition();
                    session.move_to_paragraph_edge(end, extend);
                }
                Ok(())
            }
            TextEdit::WordStep { forward, extend } => {
                if let Some(session) = &mut self.session {
                    session.commit_composition();
                    session.move_by_word(forward, extend);
                }
                Ok(())
            }
            TextEdit::SelectAll => {
                // Card 029 note: a live composition keeps its own anchor/
                // caret (the IME owns its preedit), so select-all does not
                // commit it first — the next commit resets anchor/caret to
                // the preedit range. Deliberate: the IME's caret wins.
                if let Some(session) = &mut self.session {
                    session.select_all();
                }
                Ok(())
            }
            // Card 028: pasted text replaces the selection (or inserts at
            // the caret) and takes the insertion point's style — the same
            // span-shifting rules as typed text.
            // Card 029: the IME routes its preedit and commit through the
            // session; an empty `Ime::Preedit` clears the composition. Each
            // arm rides the draft outbox where the draft changes.
            TextEdit::SetComposition(text) => {
                let Some(session) = &mut self.session else {
                    return Err(ToolError::NotStarted);
                };
                session.set_composition(text);
                let session = session.clone();
                ctx.emit_request(ToolRequest::TextDraft {
                    layer: session.layer,
                    kind: Box::new(Self::payload(&session)),
                });
                Ok(())
            }
            TextEdit::CommitIme(text) => {
                let Some(session) = &mut self.session else {
                    return Err(ToolError::NotStarted);
                };
                let was_composing = session.composition.is_some();
                let empty = text.is_empty();
                session.clear_composition();
                if !was_composing && empty {
                    // Nothing to remove and nothing to insert: no draft
                    // change, no outbox traffic (matches ClearComposition's
                    // no-op contract).
                    return Ok(());
                }
                if !text.is_empty() {
                    session.replace_selection(text);
                }
                let session = session.clone();
                ctx.emit_request(ToolRequest::TextDraft {
                    layer: session.layer,
                    kind: Box::new(Self::payload(&session)),
                });
                Ok(())
            }
            TextEdit::ClearComposition => {
                let Some(session) = &mut self.session else {
                    return Err(ToolError::NotStarted);
                };
                if session.composition.is_none() {
                    return Ok(());
                }
                session.clear_composition();
                let session = session.clone();
                ctx.emit_request(ToolRequest::TextDraft {
                    layer: session.layer,
                    kind: Box::new(Self::payload(&session)),
                });
                Ok(())
            }
            // Card 032: resizing the box reflows the draft — the type size
            // and the layer transform are untouched. The width clamps to the
            // same minimum a drag-creation respects; non-finite values are
            // refused, not clamped into the model.
            TextEdit::ResizeBox { width, height } => {
                let Some(session) = &mut self.session else {
                    return Err(ToolError::NotStarted);
                };
                if !width.is_finite()
                    || width < PARAGRAPH_MIN_BOX_WIDTH_PX
                    || width > PARAGRAPH_MAX_BOX_WIDTH_PX
                {
                    return Err(ToolError::NotFinite {
                        what: "box width",
                        value: width,
                    });
                }
                if let Some(h) = height {
                    // A negative height would pass tool validation and be
                    // refused by the document — the draft would diverge from
                    // the layer and typing would go invisible. Refuse here.
                    if !h.is_finite() || h < 0.0 {
                        return Err(ToolError::NotFinite {
                            what: "box height",
                            value: h,
                        });
                    }
                }
                let before = session.draft.frame;
                let next = Frame::Box { width, height };
                if before == next {
                    return Ok(());
                }
                session.draft.frame = next;
                let session = session.clone();
                ctx.emit_request(ToolRequest::TextDraft {
                    layer: session.layer,
                    kind: Box::new(Self::payload(&session)),
                });
                Ok(())
            }
            TextEdit::PasteText(text) => {
                let Some(session) = &mut self.session else {
                    return Err(ToolError::NotStarted);
                };
                if text.is_empty() {
                    return Ok(());
                }
                // Paste is the only unbounded text source: refuse before the
                // session mutates, or the draft would diverge from the
                // document's own MAX_TEXT_BYTES validation. The selected
                // range goes away with the paste, so it does not count.
                let selected = session.selection().end - session.selection().start;
                if session.draft.text.len() - selected + text.len()
                    > layer_model::text::MAX_TEXT_BYTES
                {
                    return Err(ToolError::PasteTooLarge {
                        bytes: text.len(),
                        max: layer_model::text::MAX_TEXT_BYTES,
                    });
                }
                session.replace_selection(text);
                let session = session.clone();
                ctx.emit_request(ToolRequest::TextDraft {
                    layer: session.layer,
                    kind: Box::new(Self::payload(&session)),
                });
                Ok(())
            }
            // Card 028: forward deletion changes the draft — it rides the
            // draft outbox like insert/backspace.
            TextEdit::DeleteForward => {
                let Some(session) = &mut self.session else {
                    return Err(ToolError::NotStarted);
                };
                if !session.delete_forward() {
                    return Ok(());
                }
                let session = session.clone();
                ctx.emit_request(ToolRequest::TextDraft {
                    layer: session.layer,
                    kind: Box::new(Self::payload(&session)),
                });
                Ok(())
            }
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
    fn typing_renders_through_the_draft_outbox_and_confirm_is_one_entry() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = TypeTool::default();
        click(&mut tool, &mut ctx, Vec2::new(10.0, 10.0));
        let layer = tool.session().unwrap().layer;
        let _ = ctx.drain();

        tool.insert(&mut ctx, "Hi").unwrap();
        tool.insert(&mut ctx, "!").unwrap();
        assert_eq!(tool.session().unwrap().draft.text, "Hi!");
        assert!(ctx.commands().is_empty(), "keystrokes are not history");
        tool.backspace(&mut ctx).unwrap();
        assert_eq!(tool.session().unwrap().draft.text, "Hi");
        let drafts = ctx.drain_requests();
        assert_eq!(drafts.len(), 3, "one draft per keystroke: {drafts:?}");
        for request in &drafts {
            let ToolRequest::TextDraft {
                layer: draft_layer,
                kind,
            } = request
            else {
                panic!("a keystroke emitted {request:?}");
            };
            assert_eq!(*draft_layer, layer);
            let LayerKind::Text(run) = &**kind else {
                panic!("a draft carried {kind:?}");
            };
            assert_eq!(run.size_px, DEFAULT_SIZE_PX);
        }

        // Confirm reconciles through one TextConfirm request: the shell
        // restores the original and then lands the SetLayerKind, whose
        // inverse is the original — one undo takes the whole run back.
        tool.confirm(&mut ctx);
        assert!(
            ctx.commands().is_empty(),
            "confirm rides the request outbox"
        );
        let requests = ctx.drain_requests();
        assert_eq!(requests.len(), 1, "{requests:?}");
        let ToolRequest::TextConfirm {
            layer: confirm_layer,
            original,
            draft,
        } = &requests[0]
        else {
            panic!("confirm emitted {requests:?}");
        };
        assert_eq!(*confirm_layer, layer);
        let LayerKind::Text(run) = &**draft else {
            unreachable!()
        };
        assert_eq!(run.text, "Hi");
        let LayerKind::Text(restored) = &**original else {
            unreachable!()
        };
        assert_eq!(restored.text, "", "the original is the pre-session payload");
        assert!(!tool.is_editing());
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
            tool.session().unwrap().draft.text,
            "",
            "the second layer inherited the first one's run"
        );
        let creates = ctx
            .commands()
            .iter()
            .filter(|c| matches!(c, Command::CreateLayer { .. }))
            .count();
        assert_eq!(creates, 2);
        // Card 025: the second click CONFIRMED the first session — its typed
        // draft lands as one history entry instead of stranding on the layer.
        let requests = ctx.drain_requests();
        let confirmed = requests
            .iter()
            .filter(|r| matches!(r, ToolRequest::TextConfirm { .. }))
            .count();
        assert_eq!(confirmed, 1, "the outgoing run was confirmed: {requests:?}");
        let ToolRequest::TextConfirm {
            layer: confirmed_layer,
            draft,
            ..
        } = requests
            .iter()
            .find(|r| matches!(r, ToolRequest::TextConfirm { .. }))
            .expect("just counted it")
        else {
            unreachable!("just counted it")
        };
        assert_eq!(*confirmed_layer, first);
        let LayerKind::Text(run) = &**draft else {
            unreachable!()
        };
        assert_eq!(run.text, "one", "the first run's draft is what commits");
    }

    #[test]
    fn confirm_commits_and_the_cancel_contract_stays_silent() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = TypeTool::default();
        click(&mut tool, &mut ctx, Vec2::new(1.0, 2.0));
        let _ = ctx.drain();
        tool.insert(&mut ctx, "kept").unwrap();
        let _ = ctx.drain_requests();

        // Enter (Tool::commit) confirms: one TextConfirm request.
        assert!(tool.has_pending_commit());
        Tool::commit(&mut tool, &mut ctx).unwrap();
        assert!(!tool.is_editing());
        assert!(!tool.is_active());
        assert!(
            ctx.commands().is_empty(),
            "confirm rides the request outbox"
        );
        let requests = ctx.drain_requests();
        assert_eq!(requests.len(), 1, "{requests:?}");
        assert!(matches!(&requests[0], ToolRequest::TextConfirm { .. }));

        // The Tool::cancel contract (a dropped gesture with no drained
        // outbox) still emits nothing; the rich cancel rides TextEdit::Cancel.
        click(&mut tool, &mut ctx, Vec2::new(3.0, 4.0));
        let _ = ctx.drain();
        Tool::cancel(&mut tool, &mut ctx);
        assert!(!tool.is_editing());
        assert!(
            ctx.commands().is_empty() && ctx.drain_requests().is_empty(),
            "the cancel contract emits nothing"
        );
    }

    // -- card 025: rich session state --------------------------------------

    #[test]
    fn the_session_represents_a_mid_string_caret_and_a_selected_range() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = TypeTool::default();
        click(&mut tool, &mut ctx, Vec2::new(0.0, 0.0));
        let _ = ctx.drain();
        tool.insert(&mut ctx, "Hello World").unwrap();

        {
            let session = tool.session().unwrap();
            assert_eq!(session.caret, 11);
            assert!(!session.has_selection());
        }

        // A caret in the middle: byte index 5 sits between the words, and the
        // anchor follows so the selection stays empty.
        tool.session.as_mut().unwrap().place_caret(5);
        {
            let session = tool.session().unwrap();
            assert_eq!(session.caret, 5);
            assert_eq!(session.anchor, 5);
            assert_eq!(session.selection(), 5..5);
        }

        // A selected range, either direction of dragging.
        tool.session.as_mut().unwrap().select(8, 2);
        let session = tool.session().unwrap();
        assert!(session.has_selection());
        assert_eq!(session.selection(), 2..8, "the range is ordered");
    }

    #[test]
    fn inserting_mid_string_replaces_the_selection_and_keeps_spans_valid() {
        let span = |start: usize, end: usize| layer_model::text::StyleSpan {
            start,
            end,
            style: Default::default(),
        };
        let mut session = TextSession {
            layer: LayerId::new(),
            origin: Vec2::ZERO,
            original: TextLayer::default(),
            draft: TextLayer {
                text: "one two three".to_string(),
                spans: vec![span(0, 3), span(8, 13)],
                ..TextLayer::default()
            },
            caret: 0,
            anchor: 0,
            composition: None,
            created_layer: true,
        };
        // Select "two" (bytes 4..7) and replace it with a longer word.
        session.select(4, 7);
        let inserted = session.replace_selection("TWELVE");
        assert_eq!(inserted, 6);
        assert_eq!(session.draft.text, "one TWELVE three");
        assert_eq!(session.caret, 10, "the caret rests after the insertion");
        assert!(!session.has_selection());
        assert_eq!(
            session.draft.spans[0].start..session.draft.spans[0].end,
            0..3,
            "a span before the edit is untouched"
        );
        assert_eq!(
            session.draft.spans[1].start..session.draft.spans[1].end,
            11..16,
            "a span after the edit shifts whole"
        );
        assert!(
            session.draft.validate().is_ok(),
            "the draft still validates"
        );

        // Deleting a range pulls the spans back (the delete removes exactly
        // the inserted word; both surrounding spaces stay).
        session.select(4, 10);
        assert!(session.delete_selection());
        assert_eq!(session.draft.text, "one  three");
        assert_eq!(
            session.draft.spans[1].start..session.draft.spans[1].end,
            5..10
        );
        assert!(session.draft.validate().is_ok());

        // A caret can never land inside a multi-byte character: a request
        // inside the four-byte fox clamps back to its start, past it clamps
        // to the text's end.
        let fox = '\u{1F98A}'.to_string();
        let mut wide = TextSession {
            layer: LayerId::new(),
            origin: Vec2::ZERO,
            original: TextLayer::default(),
            draft: TextLayer {
                text: format!("{fox}x"),
                ..TextLayer::default()
            },
            caret: 0,
            anchor: 0,
            composition: None,
            created_layer: true,
        };
        wide.place_caret(2);
        assert_eq!(wide.caret, 0, "clamped back to the fox's start");
        wide.place_caret(5);
        assert_eq!(wide.caret, 5, "the end is a boundary");
        wide.place_caret(99);
        assert_eq!(wide.caret, 5, "clamped to the text's length");
    }

    #[test]
    fn cancel_restores_the_original_and_removes_a_created_layer() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = TypeTool::default();

        // A click-created run: cancel deletes the layer (no stray layer).
        click(&mut tool, &mut ctx, Vec2::new(2.0, 2.0));
        let _ = ctx.drain();
        tool.insert(&mut ctx, "draft").unwrap();
        let _ = ctx.drain_requests();
        tool.cancel_session(&mut ctx);
        let commands = ctx.drain();
        assert_eq!(commands.len(), 1, "{commands:?}");
        assert!(
            matches!(&commands[0], Command::DeleteLayer { .. }),
            "the created layer is deleted: {commands:?}"
        );
        assert!(!tool.is_editing());

        // An entered run (card 026 will route it; the semantics live here):
        // cancel restores the original payload through the draft outbox —
        // no history entry at all.
        let original = TextLayer {
            text: "The Headline".to_string(),
            font_family: "DejaVu Sans".to_string(),
            size_px: 48.0,
            style: layer_model::text::BaseStyle {
                weight: layer_model::text::Weight::BOLD,
                ..layer_model::text::BaseStyle::default()
            },
            ..TextLayer::default()
        };
        tool.session = Some(TextSession {
            layer: LayerId::new(),
            origin: Vec2::ZERO,
            original: original.clone(),
            draft: TextLayer {
                text: "Edited".to_string(),
                ..TextLayer::default()
            },
            caret: 6,
            anchor: 6,
            composition: None,
            created_layer: false,
        });
        tool.cancel_session(&mut ctx);
        assert!(ctx.drain().is_empty(), "cancel leaves no history debris");
        let requests = ctx.drain_requests();
        let ToolRequest::TextDraft { kind, .. } = &requests[0] else {
            panic!("cancel emitted {requests:?}");
        };
        let LayerKind::Text(restored) = &**kind else {
            unreachable!()
        };
        assert_eq!(restored, &original, "the original payload, styles included");
        assert!(!tool.is_editing());
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

    #[test]
    fn a_deletion_that_swallows_a_span_start_stays_valid() {
        // The reviewer's repro: the deleted range covers a span's start, so
        // the naive "keep the start" rule would leave the span past its own
        // text — exactly what validate refuses. The start collapses to the
        // edit point instead.
        let span = |start: usize, end: usize| layer_model::text::StyleSpan {
            start,
            end,
            style: Default::default(),
        };
        let mut session = TextSession {
            layer: LayerId::new(),
            origin: Vec2::ZERO,
            original: TextLayer::default(),
            draft: TextLayer {
                text: "abcdefgh".to_string(),
                spans: vec![span(5, 8)],
                ..TextLayer::default()
            },
            caret: 0,
            anchor: 0,
            composition: None,
            created_layer: true,
        };
        session.select(2, 6);
        session.delete_selection();
        assert_eq!(session.draft.text, "abgh");
        let span = &session.draft.spans[0];
        assert!(
            span.start <= span.end && span.end <= session.draft.text.len(),
            "the span stayed inside its own text: {span:?}"
        );
        assert!(session.draft.validate().is_ok());
    }

    #[test]
    fn the_composition_is_replaced_wholesale_and_can_be_committed_or_cleared() {
        let mut session = TextSession {
            layer: LayerId::new(),
            origin: Vec2::ZERO,
            original: TextLayer::default(),
            draft: TextLayer {
                text: "ok ".to_string(),
                ..TextLayer::default()
            },
            caret: 3,
            anchor: 3,
            composition: None,
            created_layer: true,
        };
        // Successive preedit updates replace, never accumulate.
        session.set_composition("n");
        assert_eq!(session.draft.text, "ok n");
        session.set_composition("ni");
        assert_eq!(session.draft.text, "ok ni", "the old preedit went with it");
        let composition = session.composition.as_ref().unwrap();
        assert_eq!(composition.range, 3..5);

        // Commit fixes the preedit as ordinary text.
        session.commit_composition();
        assert!(session.composition.is_none());
        assert_eq!(session.caret, 5);

        // A cleared composition removes its bytes and restores the caret.
        session.set_composition("hao");
        assert_eq!(session.draft.text, "ok nihao");
        session.clear_composition();
        assert_eq!(session.draft.text, "ok ni", "the preedit went with it");
        assert_eq!(session.caret, 5, "the caret is where the composition began");
    }

    // -- card 027: caret movement and range selection ----------------------

    /// A session over `text` with the caret where the caller wants it.
    fn session_over(text: &str, caret: usize) -> TextSession {
        TextSession {
            layer: LayerId::new(),
            origin: Vec2::ZERO,
            original: TextLayer::default(),
            draft: TextLayer {
                text: text.to_string(),
                ..TextLayer::default()
            },
            caret,
            anchor: caret,
            composition: None,
            created_layer: true,
        }
    }

    #[test]
    fn movement_never_splits_utf8_and_selections_stay_valid() {
        // Accented + emoji + CJK: every step lands on a char boundary, and a
        // selection is always a valid range of the draft.
        let fox = '\u{1F98A}';
        let e_acute = "\u{00E9}";
        let text = format!("caf{e_acute}{fox}x"); // e-acute is 2 bytes, the fox 4
        let mut session = session_over(&text, text.len());
        let total = text.len();

        // Walk left to the start: five characters, and never inside one.
        let mut visits = vec![session.caret];
        while session.caret > 0 {
            session.move_horizontally(-1, false);
            visits.push(session.caret);
            assert!(text.is_char_boundary(session.caret));
            let range = session.selection();
            assert!(range.end <= text.len() && range.start <= range.end);
        }
        assert_eq!(session.caret, 0);
        // One visit per character: the fox is a single step, é is one step.
        assert_eq!(
            visits,
            vec![10, 9, 5, 3, 2, 1, 0],
            "every step whole: {visits:?}"
        );

        // Walk right again: back to the end, still whole.
        for _ in 0..total + 3 {
            session.move_horizontally(1, false);
            assert!(text.is_char_boundary(session.caret));
        }
        assert_eq!(session.caret, total, "clamped at the end");

        // Shift-selection: the anchor stays while the caret moves, and the
        // range covers the fox whole.
        session.place_caret(3); // the accented character's start
        session.move_horizontally(1, true);
        assert_eq!(
            session.selection(),
            3..5,
            "the accented char is one step, whole"
        );
        session.move_horizontally(1, true);
        assert_eq!(session.selection(), 3..9, "the fox is the next step, whole");
        // Collapsing clears the selection.
        session.move_horizontally(-1, false);
        assert!(!session.has_selection());
        assert_eq!(session.caret, 5, "back to the fox's start");
    }

    #[test]
    fn paragraph_edges_word_steps_and_select_all_behave() {
        let mut session = session_over("one\ntwo three\nfour", 8); // caret inside "three"
                                                                   // Home: the paragraph's start, not the text's.
        session.move_to_paragraph_edge(false, false);
        assert_eq!(session.caret, 4, "start of the second paragraph");
        // End: the paragraph's end.
        session.move_to_paragraph_edge(true, false);
        assert_eq!(session.caret, 13, "end of the second paragraph");
        // Shift+Home extends back over the paragraph.
        session.move_to_paragraph_edge(false, true);
        assert_eq!(session.selection(), 4..13);

        // Word steps: from inside "three", forward crosses the break to the
        // next word's start (the step skips the trailing whitespace).
        session.place_caret(9);
        session.move_by_word(true, false);
        assert_eq!(session.caret, 14, "to the next word's start");
        session.move_by_word(true, false);
        assert_eq!(session.caret, 18, "on to the last word's end");
        session.move_by_word(false, false);
        assert_eq!(session.caret, 14, "back to the last word's start");
        session.move_by_word(false, false);
        assert_eq!(session.caret, 8, "back to the word's start");

        // Multi-byte words move whole: the fox is one word step.
        let fox = '\u{1F98A}';
        let mut wide = session_over(&format!("{fox}{fox} tail"), 8);
        wide.move_by_word(false, false);
        assert_eq!(wide.caret, 0, "both foxes are one word step back");

        // Select all.
        session.select_all();
        assert_eq!(session.selection(), 0..18);
        assert!(session.has_selection());
    }

    #[test]
    fn the_movement_edit_variants_reach_the_session() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = TypeTool::default();
        click(&mut tool, &mut ctx, Vec2::new(0.0, 0.0));
        let _ = ctx.drain();
        tool.insert(&mut ctx, "Hello").unwrap();
        let _ = ctx.drain_requests();

        // Home, then two steps right with Shift: "He" selected.
        Tool::text_edit(
            &mut tool,
            &mut ctx,
            TextEdit::ParagraphEdge {
                end: false,
                extend: false,
            },
        )
        .unwrap();
        Tool::text_edit(
            &mut tool,
            &mut ctx,
            TextEdit::CaretStep {
                back: false,
                extend: true,
            },
        )
        .unwrap();
        Tool::text_edit(
            &mut tool,
            &mut ctx,
            TextEdit::CaretStep {
                back: false,
                extend: true,
            },
        )
        .unwrap();
        let session = tool.session().unwrap();
        assert_eq!(session.selection(), 0..2, "Shift+arrows selected He");
        assert!(ctx.commands().is_empty(), "movement is not history");
        assert!(
            ctx.drain_requests().is_empty(),
            "movement renders nothing new"
        );
    }

    #[test]
    fn deletion_is_forward_backward_and_word_wise_without_splitting_utf8() {
        let fox = '\u{1F98A}';
        let e_acute = '\u{00E9}';
        let text = format!("caf{e_acute}{fox}tail");

        // Forward delete steps over multi-byte characters whole.
        let mut session = session_over(&text, 3); // before the accented char
        assert!(session.delete_forward());
        assert_eq!(
            session.draft.text,
            format!("caf{fox}tail"),
            "the accented char went whole"
        );
        // Backward delete from after the fox takes the fox whole: the caret
        // steps behind the fox, then one backward delete removes it whole.
        session.place_caret(session.draft.text.len());
        session.move_horizontally(-1, false);
        assert!(session.draft.text.is_char_boundary(session.caret));
        session.delete_backward();
        // Word deletion: from the end, one word step back deletes the word.
        let mut wordy = session_over(&format!("one {fox} two"), 11.min(text.len() + 4));
        wordy.place_caret(wordy.draft.text.len());
        wordy.delete_word(false);
        assert_eq!(wordy.draft.text, "one \u{1F98A} ", "the last word went");
        wordy.delete_word(false);
        assert_eq!(wordy.draft.text, "one ", "the fox word went whole");
        // Forward word deletion from the start.
        let mut from_start = session_over("two words", 0);
        from_start.delete_word(true);
        assert_eq!(
            from_start.draft.text, "words",
            "the first word and its space went"
        );
        // Selection delete wins over any caret position.
        let mut ranged = session_over("keep CUT keep", 0);
        ranged.select(5, 8);
        ranged.delete_forward();
        assert_eq!(
            ranged.draft.text, "keep  keep",
            "the range went, caret parked at its start"
        );
    }

    impl TextSession {
        /// Test helper: install spans directly (the session's draft is the
        /// authority during editing).
        fn spans_setup(&mut self, spans: Vec<layer_model::text::StyleSpan>) {
            self.draft.spans = spans;
        }
    }

    #[test]
    fn insertion_inherits_surrounding_styles_and_shifts_the_spans() {
        let span = |start: usize, end: usize| layer_model::text::StyleSpan {
            start,
            end,
            style: layer_model::text::StyleOverride {
                weight: Some(layer_model::text::Weight(700)),
                ..layer_model::text::StyleOverride::default()
            },
        };
        // "bold BOLD rest": span 0..4 bold, the rest base.
        let mut session = session_over("bold BOLD rest", 0);
        session.spans_setup(vec![span(0, 4), span(5, 9)]);
        // Insert INSIDE the bold span: the span grows over the insertion —
        // the inserted characters take the span's style.
        session.place_caret(2);
        session.replace_selection("XX");
        assert_eq!(session.draft.text, "boXXld BOLD rest");
        assert_eq!(
            session.draft.spans[0].start..session.draft.spans[0].end,
            0..6,
            "the containing span grew over the inserted text"
        );
        assert!(session.draft.validate().is_ok());

        // Insert BETWEEN two styled spans: neither grows — the inserted text
        // takes the base style, and the right span shifts whole.
        let mut between = session_over("ab", 0);
        between.spans_setup(vec![span(0, 1), span(1, 2)]);
        between.place_caret(1);
        between.replace_selection("MM");
        assert_eq!(between.draft.text, "aMMb");
        let starts: Vec<usize> = between.draft.spans.iter().map(|s| s.start).collect();
        let ends: Vec<usize> = between.draft.spans.iter().map(|s| s.end).collect();
        assert_eq!(starts, vec![0, 3], "the left span kept its start");
        assert_eq!(ends, vec![1, 4], "the right span shifted whole");
        assert!(between.draft.validate().is_ok());
    }

    #[test]
    fn clipboard_paste_replaces_the_selection_and_takes_the_local_style() {
        let span = |start: usize, end: usize| layer_model::text::StyleSpan {
            start,
            end,
            style: layer_model::text::StyleOverride {
                weight: Some(layer_model::text::Weight(700)),
                ..layer_model::text::StyleOverride::default()
            },
        };
        // Paste at the caret replaces the selection; the selected text the
        // shell copies is exactly the half-open range.
        let mut session = session_over("copy me", 0);
        session.select(5, 7);
        assert_eq!(
            session.selected_text().as_deref(),
            Some("me"),
            "the copy source is the selection"
        );
        session.replace_selection("you all");
        assert_eq!(session.draft.text, "copy you all");
        assert!(
            session.selected_text().is_none(),
            "paste collapses the selection"
        );
        assert!(session.draft.validate().is_ok());

        // Pasted text inside a styled span inherits it (the grown end), like
        // typed text — one rule for both sources.
        let mut styled = session_over("bold here", 0);
        styled.spans_setup(vec![span(0, 4)]);
        styled.place_caret(2);
        styled.replace_selection("OL");
        assert_eq!(styled.draft.spans[0].start..styled.draft.spans[0].end, 0..6);
        assert!(styled.draft.validate().is_ok());
    }

    #[test]
    fn the_selection_text_accessor_dispatches_through_the_tool_trait() {
        // Card 028 review critical: the copy source must survive `Box<dyn
        // Tool>` — a session-level-only test misses exactly this wiring.
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let tool: Box<dyn Tool> = Box::new(TypeTool::default());
        assert!(
            tool.text_selection_text().is_none(),
            "no session means nothing to copy"
        );

        let mut tool = TypeTool::default();
        click(&mut tool, &mut ctx, Vec2::new(0.0, 0.0));
        tool.insert(&mut ctx, "copy me").unwrap();
        let _ = ctx.drain();
        let _ = ctx.drain_requests();
        let mut tool: Box<dyn Tool> = Box::new(tool);
        assert!(tool.text_selection_text().is_none(), "nothing selected yet");
        Tool::text_edit(tool.as_mut(), &mut ctx, TextEdit::SelectAll).unwrap();
        assert_eq!(
            tool.text_selection_text().as_deref(),
            Some("copy me"),
            "the trait dispatch reaches the session's selection"
        );
    }

    #[test]
    fn an_oversize_paste_is_refused_before_the_session_mutates() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = TypeTool::default();
        click(&mut tool, &mut ctx, Vec2::new(0.0, 0.0));
        tool.insert(&mut ctx, "keep").unwrap();
        let _ = ctx.drain();
        let _ = ctx.drain_requests();
        let oversized = "x".repeat(layer_model::text::MAX_TEXT_BYTES);
        let err = tool
            .text_edit(&mut ctx, TextEdit::PasteText(&oversized))
            .unwrap_err();
        assert!(
            err.to_string().contains("exceed"),
            "the refusal names the cap: {err}"
        );
        assert_eq!(
            tool.session.as_ref().unwrap().draft.text,
            "keep",
            "the draft is untouched"
        );
        assert!(
            ctx.drain_requests().is_empty(),
            "no draft rides out for a refused paste"
        );
    }

    #[test]
    fn ime_preedit_commit_and_withdraw_route_through_the_edit_arms() {
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = TypeTool::default();
        click(&mut tool, &mut ctx, Vec2::new(0.0, 0.0));
        let _ = ctx.drain();
        let _ = ctx.drain_requests();

        // Preedit: the draft shows the composition text; the session reports
        // composing through the trait dispatch too.
        Tool::text_edit(&mut tool, &mut ctx, TextEdit::SetComposition("nihongo")).unwrap();
        assert_eq!(tool.session.as_ref().unwrap().draft.text, "nihongo");
        assert!(tool.text_composing(), "the preedit is live");

        // A new preedit replaces the old wholesale (never accumulates).
        Tool::text_edit(&mut tool, &mut ctx, TextEdit::SetComposition("konnichiwa")).unwrap();
        assert_eq!(tool.session.as_ref().unwrap().draft.text, "konnichiwa");
        assert_eq!(
            tool.session
                .as_ref()
                .unwrap()
                .composition
                .as_ref()
                .unwrap()
                .range,
            0..10,
            "the composition range tracks the current preedit"
        );

        // Commit: the preedit becomes ordinary draft text.
        Tool::text_edit(&mut tool, &mut ctx, TextEdit::CommitIme("hello")).unwrap();
        assert_eq!(tool.session.as_ref().unwrap().draft.text, "hello");
        assert!(!tool.text_composing(), "the commit ends the composition");
        let _ = ctx.drain();

        // Withdraw: an empty preedit removes the composition text entirely.
        Tool::text_edit(&mut tool, &mut ctx, TextEdit::SetComposition("draft")).unwrap();
        assert_eq!(tool.session.as_ref().unwrap().draft.text, "hellodraft");
        Tool::text_edit(&mut tool, &mut ctx, TextEdit::ClearComposition).unwrap();
        assert_eq!(
            tool.session.as_ref().unwrap().draft.text,
            "hello",
            "the withdrawn preedit is removed whole"
        );
        assert!(!tool.text_composing());
    }

    #[test]
    fn caret_movement_commits_a_live_composition_before_its_range_goes_stale() {
        // Card 027's note folded into 029: the preedit's range would be stale
        // after any caret move, so the arms commit it first — the typed
        // preedit text stays as ordinary draft text (no data loss).
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = TypeTool::default();
        click(&mut tool, &mut ctx, Vec2::new(0.0, 0.0));
        tool.insert(&mut ctx, "ab").unwrap();
        let _ = ctx.drain();
        let _ = ctx.drain_requests();
        Tool::text_edit(&mut tool, &mut ctx, TextEdit::SetComposition("X")).unwrap();
        assert_eq!(tool.session.as_ref().unwrap().draft.text, "abX");
        assert!(tool.text_composing());

        Tool::text_edit(
            &mut tool,
            &mut ctx,
            TextEdit::CaretStep {
                back: true,
                extend: false,
            },
        )
        .unwrap();
        let session = tool.session.as_ref().unwrap();
        assert!(
            session.composition.is_none(),
            "the movement withdrew the stale preedit"
        );
        assert_eq!(session.draft.text, "abX", "the preedit text stays as text");
        assert_eq!(session.caret, 2, "the caret moved back through the preedit");

        // A fresh preedit after the move starts from the moved caret — no
        // stale range is reused.
        Tool::text_edit(&mut tool, &mut ctx, TextEdit::SetComposition("Y")).unwrap();
        assert_eq!(tool.session.as_ref().unwrap().draft.text, "abYX");
    }

    #[test]
    fn enter_as_a_newline_is_ordinary_insertion_and_commits_one_history_entry() {
        // Card 031: typing a sentence with line breaks does not produce one
        // undo per keystroke — the run commits as one transaction.
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = TypeTool::default();
        click(&mut tool, &mut ctx, Vec2::new(0.0, 0.0));
        tool.insert(&mut ctx, "line one").unwrap();
        Tool::text_edit(&mut tool, &mut ctx, TextEdit::Insert("\n")).unwrap();
        tool.insert(&mut ctx, "line two").unwrap();
        assert_eq!(
            tool.session.as_ref().unwrap().draft.text,
            "line one\nline two",
            "the newline is ordinary draft text"
        );
        assert_eq!(
            tool.session.as_ref().unwrap().draft.validate().map(|_| ()),
            Ok(()),
            "the two-paragraph draft validates"
        );
        let _ = ctx.drain();
        let _ = ctx.drain_requests();
        // Confirm: ONE TextConfirm request for the whole run — the shell's
        // restore-then-commit turns it into the cohesive transaction the
        // undo stack sees (card 025).
        Tool::text_edit(&mut tool, &mut ctx, TextEdit::Confirm).unwrap();
        let requests = ctx.drain_requests();
        assert_eq!(requests.len(), 1, "the whole run is one confirm request");
        assert!(matches!(requests[0], ToolRequest::TextConfirm { .. }));
    }

    #[test]
    fn a_type_drag_creates_paragraph_text_and_a_click_creates_point_text() {
        // Card 032: the drag's horizontal extent becomes the wrapping box's
        // width; a click (no drag) keeps the point frame.
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = TypeTool::default();

        // A click: point text.
        click(&mut tool, &mut ctx, Vec2::new(10.0, 10.0));
        let session = tool.session.as_ref().unwrap();
        assert!(
            matches!(session.draft.frame, Frame::Point),
            "a click is point text"
        );
        let _ = ctx.drain();
        tool.confirm(&mut ctx);
        let _ = ctx.drain_requests();

        // A drag: paragraph text, box width = the drag's extent (min-clamped).
        tool.pending = Some(Vec2::new(40.0, 60.0));
        tool.on_pointer_up(
            &mut ctx,
            PointerEvent {
                pos: Vec2::new(120.0, 66.0),
                pressure: 1.0,
                modifiers: crate::tool::Modifiers::default(),
            },
        )
        .unwrap();
        let session = tool.session.as_ref().unwrap();
        assert!(
            matches!(
                session.draft.frame,
                Frame::Box {
                    width: 80.0,
                    height: None
                }
            ),
            "the drag sized the box: {:?}",
            session.draft.frame
        );
        // The layer on the document carries the same frame (the draft rides
        // it only once text changes — creation emits the command directly).
        let _ = ctx.drain();
    }

    #[test]
    fn resizing_the_box_reflows_without_touching_scale_and_validates() {
        // Card 032: the box is the paragraph's geometry — resizing changes
        // the wrapping width only.
        let mut tiles = MemoryTiles::new();
        let mut ctx = ctx(&mut tiles);
        let mut tool = TypeTool::default();
        click(&mut tool, &mut ctx, Vec2::new(0.0, 0.0));
        let _ = ctx.drain();
        let _ = ctx.drain_requests();
        Tool::text_edit(
            &mut tool,
            &mut ctx,
            TextEdit::ResizeBox {
                width: 150.0,
                height: None,
            },
        )
        .unwrap();
        let session = tool.session.as_ref().unwrap();
        assert_eq!(
            session.draft.frame,
            Frame::Box {
                width: 150.0,
                height: None
            },
            "the point run converted to a box"
        );
        assert!(session.draft.validate().is_ok());
        // An exact-width change goes out; a no-change resize is a cheap no-op.
        let _ = ctx.drain_requests();
        Tool::text_edit(
            &mut tool,
            &mut ctx,
            TextEdit::ResizeBox {
                width: 150.0,
                height: None,
            },
        )
        .unwrap();
        assert!(
            ctx.drain_requests().is_empty(),
            "no draft rides for a no-op"
        );

        // Non-finite and sub-minimum widths are refused, not clamped.
        Tool::text_edit(
            &mut tool,
            &mut ctx,
            TextEdit::ResizeBox {
                width: f32::NAN,
                height: None,
            },
        )
        .unwrap_err();
        Tool::text_edit(
            &mut tool,
            &mut ctx,
            TextEdit::ResizeBox {
                width: 1.0,
                height: None,
            },
        )
        .unwrap_err();
        assert_eq!(
            tool.session.as_ref().unwrap().draft.frame,
            Frame::Box {
                width: 150.0,
                height: None
            },
            "refusals leave the frame alone"
        );
        // An explicit height lands too.
        Tool::text_edit(
            &mut tool,
            &mut ctx,
            TextEdit::ResizeBox {
                width: 120.0,
                height: Some(90.0),
            },
        )
        .unwrap();
        assert_eq!(
            tool.session.as_ref().unwrap().draft.frame,
            Frame::Box {
                width: 120.0,
                height: Some(90.0)
            }
        );
    }
}
