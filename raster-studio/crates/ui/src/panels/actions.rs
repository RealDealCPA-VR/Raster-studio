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

/// W13-E: one step of an action as the set view lists it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StepSummary {
    /// The step's name ("Gaussian Blur", "Create Layer").
    pub label: String,
    /// The step's check box: an unchecked step is not played.
    pub enabled: bool,
    /// Why Play skips this step, when this application has no equivalent
    /// for it; `None` for a step that plays.
    pub skipped: Option<String>,
}

/// W13-E: one action inside a set.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SetActionSummary {
    /// The action's index in the flat [`ActionsView::actions`] list (what
    /// [`ActionsRequest::Play`] and [`ActionSetRequest`] take).
    pub index: usize,
    pub name: String,
    pub steps: Vec<StepSummary>,
}

/// W13-E: one action set.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActionSetSummary {
    pub name: String,
    /// Whether stopped recordings join this set.
    pub recording_target: bool,
    pub actions: Vec<SetActionSummary>,
}

/// W13-E: the library as Set -> Action -> Steps, published beside the flat
/// [`ActionsView`] each frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActionSetsView {
    pub sets: Vec<ActionSetSummary>,
}

impl ActionSetsView {
    fn slot() -> egui::Id {
        egui::Id::new("raster-action-sets-view")
    }

    /// Hand the set tree to the panel for this frame and the next.
    pub fn publish(self, ctx: &egui::Context) {
        ctx.data_mut(|d| d.insert_temp(Self::slot(), self));
    }

    /// The set tree the application last published (empty before it has).
    pub fn published(ctx: &egui::Context) -> Self {
        ctx.data(|d| d.get_temp::<Self>(Self::slot()))
            .unwrap_or_default()
    }
}

/// W13-E: a set-level request from the Actions panel. Indices are set
/// indices into [`ActionSetsView::sets`], action indices into the flat
/// [`ActionsView::actions`], and step indices into that action's steps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ActionSetRequest {
    /// Make an empty set with this name; stopped recordings join it.
    NewSet(String),
    RenameSet {
        set: usize,
        name: String,
    },
    /// Remove the set and every action in it.
    DeleteSet(usize),
    /// Stopped recordings join this set.
    RecordInto(usize),
    /// Check or uncheck one step.
    ToggleStep {
        action: usize,
        step: usize,
    },
    /// Play the action from this step on.
    PlayFrom {
        action: usize,
        step: usize,
    },
    /// Write the set as an `.atn` file (the path is asked for).
    ExportSet(usize),
    /// Read an `.atn` file into a new set (the path is asked for).
    ImportAtn,
}

fn set_requests_slot() -> egui::Id {
    egui::Id::new("raster-action-set-requests")
}

/// W13-E: queue a set-level request for the application.
pub fn request_set(ctx: &egui::Context, req: ActionSetRequest) {
    ctx.data_mut(|d| {
        d.get_temp_mut_or_default::<Vec<ActionSetRequest>>(set_requests_slot())
            .push(req)
    });
}

/// W13-E: take every queued set-level request, in click order.
pub fn take_set_requests(ctx: &egui::Context) -> Vec<ActionSetRequest> {
    ctx.data_mut(|d| {
        std::mem::take(d.get_temp_mut_or_default::<Vec<ActionSetRequest>>(set_requests_slot()))
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

/// W13-E: the ids of the set tree's controls, so a headless test (and the
/// application's route tests) can find and click them.
pub mod ids {
    pub fn new_set() -> egui::Id {
        egui::Id::new("raster-action-set-new")
    }
    pub fn import_atn() -> egui::Id {
        egui::Id::new("raster-action-set-import")
    }
    pub fn rename_set() -> egui::Id {
        egui::Id::new("raster-action-set-rename")
    }
    pub fn rename_field() -> egui::Id {
        egui::Id::new("raster-action-set-rename-field")
    }
    pub fn record_into() -> egui::Id {
        egui::Id::new("raster-action-set-record-into")
    }
    pub fn export_set() -> egui::Id {
        egui::Id::new("raster-action-set-export")
    }
    pub fn delete_set() -> egui::Id {
        egui::Id::new("raster-action-set-delete")
    }
    pub fn play_from_step() -> egui::Id {
        egui::Id::new("raster-action-set-play-from")
    }
    /// The row of the set at `set`; a click selects the set.
    pub fn set_row(set: usize) -> egui::Id {
        egui::Id::new(("raster-action-set-row", set))
    }
    /// The twirl that shows or hides the set's actions.
    pub fn set_twirl(set: usize) -> egui::Id {
        egui::Id::new(("raster-action-set-twirl", set))
    }
    /// The row of step `step` of the action at flat index `action`; a click
    /// selects the step (Play from step starts there).
    pub fn step_row(action: usize, step: usize) -> egui::Id {
        egui::Id::new(("raster-action-set-step-row", action, step))
    }
    /// The step's check box: an unchecked step is not played.
    pub fn step_toggle(action: usize, step: usize) -> egui::Id {
        egui::Id::new(("raster-action-set-step-toggle", action, step))
    }
}

/// W13-E: the set tree's own view state: the selected set and step, the
/// collapsed sets, and a rename in progress.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SetsPanelState {
    pub selected_set: Option<usize>,
    /// `(flat action index, step index)`.
    pub selected_step: Option<(usize, usize)>,
    pub collapsed: Vec<usize>,
    /// The set being renamed, and whether its field still needs focus.
    pub renaming: Option<(usize, bool)>,
}

impl SetsPanelState {
    fn slot() -> egui::Id {
        egui::Id::new("raster-action-sets-panel-state")
    }

    pub fn load(ctx: &egui::Context) -> Self {
        ctx.data(|d| d.get_temp::<Self>(Self::slot()))
            .unwrap_or_default()
    }

    pub fn store(self, ctx: &egui::Context) {
        ctx.data_mut(|d| d.insert_temp(Self::slot(), self));
    }

    /// Keep the selection inside the published tree.
    pub fn clamp(&mut self, view: &ActionSetsView) {
        let sets = view.sets.len();
        if self.selected_set.is_some_and(|s| s >= sets) {
            self.selected_set = None;
        }
        if self.renaming.is_some_and(|(s, _)| s >= sets) {
            self.renaming = None;
        }
        self.collapsed.retain(|s| *s < sets);
        if let Some((a, s)) = self.selected_step {
            let exists = view
                .sets
                .iter()
                .flat_map(|set| &set.actions)
                .any(|act| act.index == a && s < act.steps.len());
            if !exists {
                self.selected_step = None;
            }
        }
    }
}

/// W13-E: draw the Set -> Action -> Steps tree the application published
/// ([`ActionSetsView`]) with its controls: New set, Rename, Record into,
/// Export as `.atn`, Load `.atn`, Delete set, each step's check box and Play
/// from the selected step. Each click queues an [`ActionSetRequest`]. The
/// Actions dock calls this under its flat list.
pub fn draw_sets(ui: &mut egui::Ui) {
    use crate::strings::tr;
    use crate::view::{
        body, hairline, hint, icon_action_id, icon_toggle_id, labelled_button, list_row_layout,
        row_layout, text_field_sized, ActionState,
    };
    use design::{current_tokens, Space};

    let ctx = ui.ctx().clone();
    let view = ActionSetsView::published(&ctx);
    let mut state = SetsPanelState::load(&ctx);
    state.clamp(&view);
    let indent = current_tokens(ui).metrics.control_height;
    let min_field = current_tokens(ui).metrics.numeric_field_width;

    ui.add_space(Space::XSmall.pt());
    hairline(ui);
    ui.label(hint(ui, tr("ui.docks.actions.sets")));
    let mut twirl = None;
    for (si, set) in view.sets.iter().enumerate() {
        let open = !state.collapsed.contains(&si);
        let renaming = state.renaming.filter(|(s, _)| *s == si);
        let selected = state.selected_set == Some(si);
        let row = list_row_layout(ui, ids::set_row(si), selected, |ui| {
            let key = if open {
                "chevron-down"
            } else {
                "chevron-right"
            };
            if icon_toggle_id(
                ui,
                key,
                true,
                tr("ui.docks.actions.set.show"),
                Some(ids::set_twirl(si)),
            )
            .clicked()
            {
                twirl = Some(si);
            }
            if let Some((_, fresh)) = renaming {
                let width = (ui.available_width() - Space::Medium.pt()).max(min_field);
                let id = ids::rename_field();
                let edit = text_field_sized(ui, id, &set.name, width);
                if fresh {
                    // Select the whole name, so typing replaces it.
                    edit.response.request_focus();
                    let mut ts =
                        egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
                    ts.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(set.name.chars().count()),
                    )));
                    ts.store(ui.ctx(), id);
                    state.renaming = Some((si, false));
                } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    state.renaming = None;
                } else if let Some(name) = edit.committed {
                    state.renaming = None;
                    let name = name.trim().to_string();
                    if name != set.name {
                        request_set(&ctx, ActionSetRequest::RenameSet { set: si, name });
                    }
                }
            } else {
                ui.label(body(ui, set.name.clone()));
            }
            if set.recording_target {
                ui.label(hint(ui, tr("ui.docks.actions.set.recording.here")));
            }
        })
        .response;
        if row.clicked() {
            state.selected_set = Some(si);
        }
        if !open {
            continue;
        }
        for action in &set.actions {
            row_layout(ui, |ui| {
                ui.add_space(indent);
                ui.label(body(ui, action.name.clone()));
            });
            for (step_i, step) in action.steps.iter().enumerate() {
                let at = (action.index, step_i);
                let selected = state.selected_step == Some(at);
                let row = list_row_layout(ui, ids::step_row(at.0, at.1), selected, |ui| {
                    ui.add_space(indent);
                    if icon_toggle_id(
                        ui,
                        "check",
                        step.enabled,
                        tr("ui.docks.actions.step.toggle"),
                        Some(ids::step_toggle(at.0, at.1)),
                    )
                    .clicked()
                    {
                        request_set(
                            &ctx,
                            ActionSetRequest::ToggleStep {
                                action: at.0,
                                step: at.1,
                            },
                        );
                    }
                    ui.label(body(ui, step.label.clone()));
                    if let Some(why) = &step.skipped {
                        let note = format!("{} {why}", tr("ui.docks.actions.step.skipped"));
                        ui.label(hint(ui, note));
                    }
                })
                .response;
                if row.clicked() {
                    state.selected_step = Some(at);
                    state.selected_set = Some(si);
                }
            }
        }
    }
    if let Some(si) = twirl {
        match state.collapsed.iter().position(|s| *s == si) {
            Some(at) => {
                state.collapsed.remove(at);
            }
            None => state.collapsed.push(si),
        }
    }

    ui.add_space(Space::XSmall.pt());
    let set = state.selected_set;
    let step = state.selected_step;
    let mut clear_selection = false;
    ui.horizontal_wrapped(|ui| {
        if labelled_button(ui, tr("ui.docks.actions.set.new"), true, ids::new_set()).clicked() {
            let name = tr("ui.docks.actions.set.default").to_string();
            request_set(&ctx, ActionSetRequest::NewSet(name));
        }
        let rename = tr("ui.docks.actions.set.rename");
        if labelled_button(ui, rename, set.is_some(), ids::rename_set()).clicked() {
            if let Some(s) = set {
                state.renaming = Some((s, true));
            }
        }
        let record = tr("ui.docks.actions.set.record");
        if labelled_button(ui, record, set.is_some(), ids::record_into()).clicked() {
            if let Some(s) = set {
                request_set(&ctx, ActionSetRequest::RecordInto(s));
            }
        }
        let export = tr("ui.docks.actions.set.export");
        if labelled_button(ui, export, set.is_some(), ids::export_set()).clicked() {
            if let Some(s) = set {
                request_set(&ctx, ActionSetRequest::ExportSet(s));
            }
        }
        let import = tr("ui.docks.actions.set.import");
        if labelled_button(ui, import, true, ids::import_atn()).clicked() {
            request_set(&ctx, ActionSetRequest::ImportAtn);
        }
        let play = tr("ui.docks.actions.play.from");
        if labelled_button(ui, play, step.is_some(), ids::play_from_step()).clicked() {
            if let Some((action, step)) = step {
                request_set(&ctx, ActionSetRequest::PlayFrom { action, step });
            }
        }
        if icon_action_id(
            ui,
            "trash",
            tr("ui.docks.actions.set.delete"),
            ActionState::enabled_if(set.is_some()),
            Some(ids::delete_set()),
        )
        .clicked()
        {
            if let Some(s) = set {
                request_set(&ctx, ActionSetRequest::DeleteSet(s));
                clear_selection = true;
            }
        }
    });
    if clear_selection {
        state.selected_set = None;
        state.selected_step = None;
    }
    state.store(&ctx);
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
    fn set_requests_queue_apart_from_the_footer_requests() {
        let ctx = egui::Context::default();
        request(&ctx, ActionsRequest::Save);
        request_set(&ctx, ActionSetRequest::NewSet("Web".into()));
        request_set(&ctx, ActionSetRequest::PlayFrom { action: 0, step: 2 });
        assert_eq!(
            take_set_requests(&ctx),
            vec![
                ActionSetRequest::NewSet("Web".into()),
                ActionSetRequest::PlayFrom { action: 0, step: 2 }
            ]
        );
        assert!(take_set_requests(&ctx).is_empty());
        assert_eq!(take_requests(&ctx), vec![ActionsRequest::Save]);
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
