//! W9-N: File > Open of a Photoshop / Photopea resource file.
//!
//! A child module of [`crate::editor`] (declared there with `#[path]`, like
//! `actions_library`), so it extends [`Editor`] without widening its
//! surface elsewhere. The parsing lives in `asset_store::resources`; this is
//! where each library lands:
//!
//! | File | Lands in |
//! |---|---|
//! | `.pat` | the pattern presets (Layer Style ▸ Pattern Overlay, the pattern tools); the first becomes the active pattern |
//! | `.grd` | the first gradient becomes the gradient tools' ramp (options bar and next stroke); every one joins the gradient editor's preset strip and is persisted in the gradient presets |
//! | `.csh` | the Custom Shape tool's Shape list (after the built-in library), persisted in the shape presets |
//! | `.aco`, `.ase` | the Swatches panel (and so the preferences file) |
//! | `.icc`, `.icm` | assigned to the active document (re-tags it; exports carry it) |
//! | `.atn` | refused with [`asset_store::resources::ATN_REFUSAL`] |
//!
//! The Swatches panel and the options bar live in the chrome's workspace,
//! not in the editor, so what they receive is queued here
//! ([`PanelImports`]) and handed over on the next frame by
//! [`Editor::sync_panel_presets`], the same per-frame hook the Swatches
//! panel's own persistence runs through.

use std::path::Path;

use asset_store::resources::{self, GradientResource, Resource, ResourceKind};

use super::{Action, ActionError, Editor, Effect};

/// What the status line adds for imported custom shapes: where to pick them.
pub const SHAPES_NOTE: &str = "pick them from the Custom Shape tool's Shape list";

/// What the status line adds for imported gradients: where to pick them.
pub const GRADIENTS_NOTE: &str =
    "the first is the gradient tools' ramp; every one is in the gradient editor's presets";

/// Imports waiting for the chrome's workspace.
#[derive(Debug, Default)]
pub struct PanelImports {
    swatches: Vec<(String, [f32; 4])>,
    gradient: Option<layer_model::Gradient>,
    /// Whether the gradients and shapes the presets file kept from earlier
    /// sessions have been handed to the pickers yet (once, on the first
    /// frame).
    presets_registered: bool,
}

/// Hand imported gradients to the gradient editor's preset strip and custom
/// shapes to the Custom Shape tool's Shape list - the two pickers a user
/// reaches them from.
pub fn register_with_pickers<'a>(
    gradients: impl IntoIterator<Item = &'a GradientResource>,
    shapes: impl IntoIterator<Item = &'a resources::ShapeResource>,
) {
    ui::dialogs::gradient_editor::register_imported_gradients(
        gradients
            .into_iter()
            .map(|g| (g.name.clone(), gradient_ramp(g))),
    );
    tools::registry::register_custom_shape_outlines(
        shapes
            .into_iter()
            .map(|s| (s.name.clone(), s.unit_svg_path())),
    );
}

/// An imported gradient as the gradient tools' ramp.
pub fn gradient_ramp(g: &GradientResource) -> layer_model::Gradient {
    let stops = g
        .stops
        .iter()
        .map(|s| layer_model::GradientStop {
            position: s.position,
            color: [s.rgb[0], s.rgb[1], s.rgb[2], 1.0],
            midpoint: s.midpoint,
        })
        .collect();
    let alpha_stops = g
        .opacity_stops
        .iter()
        .map(|s| layer_model::GradientStop {
            position: s.position,
            color: [1.0, 1.0, 1.0, s.opacity],
            midpoint: s.midpoint,
        })
        .collect();
    layer_model::Gradient {
        stops,
        alpha_stops,
        smoothness: g.smoothness,
    }
}

/// "3 patterns", "1 pattern".
fn count(n: usize, one: &str) -> String {
    if n == 1 {
        format!("1 {one}")
    } else {
        format!("{n} {one}s")
    }
}

impl Editor {
    /// Whether File > Open routes `path` to [`Self::open_resource`] rather
    /// than decoding it as an image.
    pub fn is_resource_path(path: &Path) -> bool {
        ResourceKind::of_path(path).is_some()
    }

    /// Read a resource file and add what it carries to its library. The
    /// status line says what landed; a file that yields nothing, or cannot be
    /// read, is an error naming why.
    pub fn open_resource(&mut self, path: &Path) -> Result<Effect, ActionError> {
        let fail = |e: &dyn std::fmt::Display| {
            ActionError::failed(Action::Open, format!("{}: {e}", path.display()))
        };
        let resource = resources::load(path).map_err(|e| fail(&e))?;
        let file = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (message, effect, refused) = match resource {
            Resource::Patterns(loaded) => {
                let first = loaded.items.first().map(|p| p.name.clone());
                let n = loaded.items.len();
                for pattern in loaded.items {
                    self.presets.define_pattern(pattern);
                }
                self.save_presets();
                if let Some(name) = first {
                    let _ = self.set_active_pattern(&name);
                }
                (
                    format!("Loaded {} from {file}", count(n, "pattern")),
                    Effect::Tool,
                    loaded.refused,
                )
            }
            Resource::Gradients(loaded) => {
                let first = loaded.items.first().map(gradient_ramp);
                let n = loaded.items.len();
                register_with_pickers(&loaded.items, []);
                for gradient in loaded.items {
                    self.presets.define_gradient(gradient);
                }
                self.save_presets();
                if let Some(ramp) = first {
                    self.set_gradient_ramp(ramp.clone());
                    self.panel_imports.gradient = Some(ramp);
                }
                (
                    format!(
                        "Loaded {} from {file} ({GRADIENTS_NOTE})",
                        count(n, "gradient")
                    ),
                    Effect::Tool,
                    loaded.refused,
                )
            }
            Resource::Shapes(loaded) => {
                let n = loaded.items.len();
                register_with_pickers([], &loaded.items);
                for shape in loaded.items {
                    self.presets.define_shape(shape);
                }
                self.save_presets();
                (
                    format!(
                        "Loaded {} from {file} ({SHAPES_NOTE})",
                        count(n, "custom shape")
                    ),
                    Effect::Tool,
                    loaded.refused,
                )
            }
            Resource::Swatches(loaded) => {
                let n = loaded.items.len();
                self.panel_imports
                    .swatches
                    .extend(loaded.items.into_iter().map(|s| (s.name, s.rgba)));
                (
                    format!("Loaded {} from {file} into Swatches", count(n, "swatch")),
                    Effect::Panels,
                    loaded.refused,
                )
            }
            Resource::Icc(icc) => {
                let label = icc.description.clone().unwrap_or_else(|| file.clone());
                if !icc.is_rgb() {
                    return Err(fail(&format!(
                        "“{label}” is a {} profile; only an RGB profile can be assigned",
                        String::from_utf8_lossy(&icc.data_space).trim()
                    )));
                }
                let Some(doc) = self.active_mut() else {
                    return Err(ActionError::unavailable(
                        Action::Open,
                        "open a document to assign this colour profile to",
                    ));
                };
                if doc.document.meta.color_mode != 0 {
                    return Err(fail(&"a profile can be assigned to an RGB document only"));
                }
                doc.document.meta.color_space = raster::codec::icc_profile_space(&icc.bytes);
                doc.document.mark_dirty();
                let title = doc.title().to_string();
                (
                    format!("Assigned the profile “{label}” to {title}"),
                    Effect::DocumentEdited,
                    Vec::new(),
                )
            }
        };
        self.status = Some(if refused.is_empty() {
            message
        } else {
            format!(
                "{message} ({} skipped: {})",
                refused.len(),
                refused.join("; ")
            )
        });
        self.touch();
        Ok(effect)
    }

    fn save_presets(&self) {
        if let Err(e) = self.presets.save(&self.paths.presets_file()) {
            tracing::warn!("could not write the presets: {e}");
        }
    }

    /// Hand queued imports to the chrome's workspace: swatches join the
    /// Swatches panel (a colour already there is not added twice) and an
    /// imported gradient becomes the gradient tools' ramp in the options bar.
    pub(super) fn drain_panel_imports(&mut self, w: &mut ui::Workspace) {
        if !self.panel_imports.presets_registered {
            self.panel_imports.presets_registered = true;
            register_with_pickers(self.presets.gradients(), self.presets.shapes());
        }
        for (name, rgba) in self.panel_imports.swatches.drain(..) {
            w.swatches.add(name, rgba);
        }
        if let Some(ramp) = self.panel_imports.gradient.take() {
            w.options.set_gradient(tools::ToolId::Gradient, ramp);
        }
    }
}

#[cfg(test)]
#[path = "resource_import_tests.rs"]
mod tests;
