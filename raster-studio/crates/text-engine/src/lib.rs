//! Text engine — **postponed** until raster/layers/masks are stable (Phase 3).
//!
//! This crate is intentionally a thin placeholder so the workspace graph and
//! serialization surface are stable now. The real implementation will wrap a
//! shaping stack (cosmic-text / swash) to lay out editable text layers, feed
//! glyph coverage into the tile compositor, and support font handling.
//!
//! Kept dependency-light on purpose: nothing else should depend on the text
//! engine yet, so pulling in a shaping stack later won't disturb build times
//! for the core editor.

/// Placeholder text run description; replaced by a real shaped-run model in
/// Phase 3. Present so `layer_model::TextLayer` has a stable companion type to
/// grow into.
#[derive(Debug, Clone, Default)]
pub struct TextRun {
    pub text: String,
    pub font_family: String,
    pub size_px: f32,
}

/// Returns whether the text engine is implemented. Lets the UI grey out text
/// tools cleanly until Phase 3 without special-casing elsewhere.
pub const fn is_available() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postponed_for_now() {
        assert!(!is_available());
        let _ = TextRun::default();
    }
}
