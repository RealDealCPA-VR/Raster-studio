//! W4-I: the Actions panel's model — the named actions the application keeps,
//! as the panel shows them, and the requests the panel sends back.
//!
//! The recorded commands themselves live in the application (they carry tile
//! bytes this crate never sees), so the two sides meet through egui's frame
//! data, the way [`crate::panels::properties::RasterInks`] does: the
//! application publishes an [`ActionsView`] each frame, the panel draws it,
//! and a click queues an [`ActionsRequest`] the application takes on its next
//! frame. Record / Stop / Replay keep travelling as [`crate::Intent`]s.

/// One named action, as the panel lists it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActionSummary {
    pub name: String,
    /// One label per recorded step, in the order they replay.
    pub steps: Vec<String>,
}

/// What the Actions panel draws: the library and whether a recording runs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActionsView {
    pub recording: bool,
    pub actions: Vec<ActionSummary>,
}

impl ActionsView {
    fn slot() -> egui::Id {
        egui::Id::new("raster-actions-view")
    }

    /// Hand the library to the panel for this frame and the next.
    pub fn publish(self, ctx: &egui::Context) {
        ctx.data_mut(|d| d.insert_temp(Self::slot(), self));
    }

    /// The library the application last published (empty before it has).
    pub fn published(ctx: &egui::Context) -> Self {
        ctx.data(|d| d.get_temp::<Self>(Self::slot()))
            .unwrap_or_default()
    }
}

/// A click in the Actions panel the application performs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionsRequest {
    /// Replay the named action at this index on the active document.
    Play(usize),
    /// Remove the action at this index from the library.
    Delete(usize),
    /// Write the whole library to the actions file.
    Save,
    /// Read the actions file, adding every action not already listed.
    Load,
}

fn requests_slot() -> egui::Id {
    egui::Id::new("raster-actions-requests")
}

/// Queue a request for the application.
pub fn request(ctx: &egui::Context, req: ActionsRequest) {
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Vec<ActionsRequest>>(requests_slot())
            .push(req)
    });
}

/// Take every queued request, in click order.
pub fn take_requests(ctx: &egui::Context) -> Vec<ActionsRequest> {
    ctx.data_mut(|d| {
        std::mem::take(d.get_temp_mut_or_default::<Vec<ActionsRequest>>(requests_slot()))
    })
}

/// The panel's own view state: which action is selected and which are
/// expanded to show their steps.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActionsPanelState {
    pub selected: Option<usize>,
    pub expanded: Vec<usize>,
}

impl ActionsPanelState {
    fn slot() -> egui::Id {
        egui::Id::new("raster-actions-panel-state")
    }

    pub fn load(ctx: &egui::Context) -> Self {
        ctx.data(|d| d.get_temp::<Self>(Self::slot()))
            .unwrap_or_default()
    }

    pub fn store(self, ctx: &egui::Context) {
        ctx.data_mut(|d| d.insert_temp(Self::slot(), self));
    }

    pub fn is_expanded(&self, index: usize) -> bool {
        self.expanded.contains(&index)
    }

    pub fn toggle(&mut self, index: usize) {
        match self.expanded.iter().position(|i| *i == index) {
            Some(at) => {
                self.expanded.remove(at);
            }
            None => self.expanded.push(index),
        }
    }

    /// Keep the selection and the expanded rows inside a library of `len`.
    pub fn clamp(&mut self, len: usize) {
        if self.selected.is_some_and(|i| i >= len) {
            self.selected = len.checked_sub(1);
        }
        self.expanded.retain(|i| *i < len);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_queue_in_order_and_are_taken_once() {
        let ctx = egui::Context::default();
        request(&ctx, ActionsRequest::Play(1));
        request(&ctx, ActionsRequest::Save);
        assert_eq!(
            take_requests(&ctx),
            vec![ActionsRequest::Play(1), ActionsRequest::Save]
        );
        assert!(take_requests(&ctx).is_empty());
    }

    #[test]
    fn the_panel_state_clamps_to_a_shrunk_library() {
        let mut s = ActionsPanelState {
            selected: Some(3),
            expanded: vec![0, 3],
        };
        s.clamp(2);
        assert_eq!(s.selected, Some(1));
        assert_eq!(s.expanded, vec![0]);
    }
}
