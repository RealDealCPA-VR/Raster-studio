//! The localization catalogue: user-facing strings resolve through here,
//! keyed by locale, instead of sitting as literals in the view and dialog
//! modules.
//!
//! # How a string moves in
//!
//! A literal `"Zoom in"` becomes `tr(STR_ZOOM_IN)` — a stable key constant and
//! a table row per locale. The lookup is a static map: `en` is the source of
//! truth and always complete; any other locale falls back to the English
//! string rather than showing a key, so a partial translation ships.
//!
//! # The migration is a wave, not a commit
//!
//! `crates/ui/src/view` and `crates/ui/src/dialogs` hold ~1600 string
//! literals. `the_catalogue_resolves_every_registered_key_for_every_locale`
//! proves the table itself is sound; the no-literal lint over those modules
//! (`P3.12`'s validate) turns red for the first time the day the last literal
//! moves, and until then it is recorded as not-yet-passing rather than
//! quietly weakened.

use std::sync::atomic::{AtomicU8, Ordering};

/// The languages the catalogue carries. `En` is the source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Locale {
    #[default]
    En,
}

impl Locale {
    /// Every locale the catalogue has a row for, in preferences-list order.
    /// One entry today: the Preferences dialog offers exactly this list, so
    /// it cannot promise a language the table cannot show.
    pub const ALL: &'static [Locale] = &[Locale::En];

    /// The locale the editor shows, as the preferences system stores it.
    pub fn from_code(code: &str) -> Self {
        Self::ALL
            .iter()
            .copied()
            .find(|l| l.code() == code)
            .unwrap_or(Self::En)
    }

    /// The BCP-47 code, for the preferences UI.
    pub const fn code(self) -> &'static str {
        match self {
            Self::En => "en",
        }
    }

    /// The name shown in the preferences list, in that language.
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::En => "English",
        }
    }
}

/// The active locale. One per process: the editor is a single-window app and
/// the choice lives in preferences, read once at startup.
static ACTIVE: AtomicU8 = AtomicU8::new(0);

/// Switch the catalogue's locale. Unknown codes fall back to English.
pub fn set_locale(locale: Locale) {
    let index = Locale::ALL.iter().position(|l| *l == locale).unwrap_or(0);
    ACTIVE.store(index as u8, Ordering::Relaxed);
}

/// The locale in force.
pub fn active() -> Locale {
    let index = ACTIVE.load(Ordering::Relaxed) as usize;
    Locale::ALL.get(index).copied().unwrap_or(Locale::En)
}

/// Every catalogue entry: the English source string first, then any
/// translations. A locale missing from a row falls back to English at lookup.
const TABLE: &[(&str, &[(Locale, &str)])] = &[
    // W3-H: Color Range, Select ▸ Modify, Save / Load Selection.
    ("ui.selection_modify.border.title", &[(Locale::En, "Border Selection")]),
    ("ui.selection_modify.smooth.title", &[(Locale::En, "Smooth Selection")]),
    ("ui.selection_modify.expand.title", &[(Locale::En, "Expand Selection")]),
    ("ui.selection_modify.contract.title", &[(Locale::En, "Contract Selection")]),
    ("ui.selection_modify.feather.title", &[(Locale::En, "Feather Selection")]),
    ("ui.selection_modify.width", &[(Locale::En, "Width")]),
    ("ui.selection_modify.sample.radius", &[(Locale::En, "Sample radius")]),
    ("ui.selection_modify.expand.by", &[(Locale::En, "Expand by")]),
    ("ui.selection_modify.contract.by", &[(Locale::En, "Contract by")]),
    ("ui.selection_modify.feather.radius", &[(Locale::En, "Feather radius")]),
    ("ui.selection_modify.apply", &[(Locale::En, "Apply")]),
    ("ui.selection_modify.px", &[(Locale::En, "px")]),
    ("ui.selection_modify.out.of.range", &[(Locale::En, "The amount must be within")]),
    ("ui.selection_modify.range.to", &[(Locale::En, "to")]),
    ("ui.selection_modify.border.caption", &[(Locale::En, "Selects a band of this width along the selection's edge")]),
    ("ui.selection_name.alpha", &[(Locale::En, "Alpha")]),
    ("ui.selection_name.save.title", &[(Locale::En, "Save Selection")]),
    ("ui.selection_name.load.title", &[(Locale::En, "Load Selection")]),
    ("ui.selection_name.empty", &[(Locale::En, "The name cannot be empty")]),
    ("ui.selection_name.taken", &[(Locale::En, "A saved selection already has that name")]),
    ("ui.selection_name.name", &[(Locale::En, "Name")]),
    ("ui.selection_name.save.caption", &[(Locale::En, "The selection is kept with the document under this name")]),
    ("ui.selection_name.save", &[(Locale::En, "Save")]),
    ("ui.selection_name.op.new", &[(Locale::En, "New Selection")]),
    ("ui.selection_name.op.add", &[(Locale::En, "Add to Selection")]),
    ("ui.selection_name.op.subtract", &[(Locale::En, "Subtract from Selection")]),
    ("ui.selection_name.op.intersect", &[(Locale::En, "Intersect with Selection")]),
    ("ui.selection_name.op.needs.selection", &[(Locale::En, "There is no live selection to combine with")]),
    ("ui.selection_name.none.saved", &[(Locale::En, "No selection has been saved")]),
    ("ui.selection_name.channel", &[(Locale::En, "Channel")]),
    ("ui.selection_name.operation", &[(Locale::En, "Operation")]),
    ("ui.selection_name.invert", &[(Locale::En, "Invert")]),
    ("ui.selection_name.load", &[(Locale::En, "Load")]),
    ("ui.color_range.title", &[(Locale::En, "Color Range")]),
    ("ui.color_range.subtitle", &[(Locale::En, "Select every pixel near one colour")]),
    ("ui.color_range.view.selection", &[(Locale::En, "Selection")]),
    ("ui.color_range.view.image", &[(Locale::En, "Image")]),
    ("ui.color_range.sampled.colour", &[(Locale::En, "Sampled colour")]),
    ("ui.color_range.fuzziness", &[(Locale::En, "Fuzziness")]),
    ("ui.color_range.invert", &[(Locale::En, "Invert")]),
    ("ui.color_range.click.to.sample", &[(Locale::En, "Click anywhere to sample a colour")]),
    ("ui.color_range.click.preview", &[(Locale::En, "Click the preview to sample a colour from it")]),
    ("ui.color_range.select", &[(Locale::En, "Select")]),
    ("ui.color_range.eyedropper", &[(Locale::En, "Eyedropper")]),
    ("actions.record", &[(Locale::En, "Record")]),
    ("ui.canvas_rotation.rotates.everything", &[(Locale::En, "Rotates the canvas and every layer. Right angles are pixel-exact; other angles resample.")]),
    ("ui.docks.enter.a.colour", &[(Locale::En, "Enter a colour like #3366CC")]),
    ("ui.docks.no.layers.yet", &[(Locale::En, "No layers yet. Add one with the + button below.")]),
    ("ui.docks.show.hide.layer", &[(Locale::En, "Show / hide layer")]),
    ("ui.docks.show.hide.channel", &[(Locale::En, "Show / hide this channel")]),
    ("ui.docks.show.hide.path", &[(Locale::En, "Show / hide this path")]),
    ("ui.toolbar.background.picker", &[(Locale::En, "Background — double-click for the picker")]),
    ("ui.toolbar.gradient.stops", &[(Locale::En, "Edit gradient stops — click to open the editor")]),
    ("ui.toolbar.foreground.picker", &[(Locale::En, "Foreground — double-click for the picker")]),
    ("ui.canvas_size.smaller.clips", &[(Locale::En, "The new canvas is smaller — content outside it will be clipped.")]),
    ("ui.color_picker.before.after", &[(Locale::En, "before / after")]),
    ("ui.export_as.16.bit", &[(Locale::En, "16 bit")]),
    ("ui.export_as.8.bit", &[(Locale::En, "8 bit")]),
    ("ui.export_as.exif.not.implemented", &[(Locale::En, "EXIF and XMP writing is not implemented — only ICC is embedded")]),
    ("ui.fill_stroke.50.grey", &[(Locale::En, "50% Grey")]),
    ("ui.fill_stroke.opacity.range", &[(Locale::En, "Opacity must be between 0% and 100%")]),
    ("ui.filter_gallery.pick.a.filter", &[(Locale::En, "Pick a filter; it applies at its default settings.")]),
    ("ui.image_size.hard.edges", &[(Locale::En, "Hard edges, no blending. Pixel art only — it aliases on downscale.")]),
    ("ui.layer_style.bevel.emboss", &[(Locale::En, "Bevel & Emboss")]),
    ("ui.layer_style.no.pattern", &[(Locale::En, "No pattern chosen — the overlay paints nothing.")]),
    ("ui.units.0.bytes", &[(Locale::En, "0 bytes")]),
    ("ui.preferences.minutes.0.is.off", &[(Locale::En, "minutes (0 is off)")]),
    ("ui.preferences.ui.scale", &[(Locale::En, "UI scale")]),
    ("ui.preferences.language", &[(Locale::En, "Language")]),
    ("ui.preferences.only.english", &[(Locale::En, "Only English is available in this build")]),
    ("ui.preferences.units", &[(Locale::En, "Units")]),
    ("ui.preferences.units.caption", &[(Locale::En, "The rulers and the size readouts use this unit")]),
    ("ui.preferences.scroll.wheel.zooms.instead.of.scrolling", &[(Locale::En, "Scroll wheel zooms instead of scrolling")]),
    ("ui.preferences.scroll.wheel.caption", &[(Locale::En, "Off: the wheel pans the view and Ctrl+wheel zooms")]),
    ("ui.preferences.scratch.directory", &[(Locale::En, "Scratch directory")]),
    ("ui.preferences.scratch.caption", &[(Locale::En, "Autosaves of never-saved documents go here; empty uses the default")]),
    ("ui.preferences.press.a.key", &[(Locale::En, "Press a key\u{2026}")]),
    ("ui.preferences.add.shortcut", &[(Locale::En, "Add shortcut")]),
    ("ui.preferences.remove.this.shortcut", &[(Locale::En, "Remove this shortcut")]),
    ("ui.preferences.changed", &[(Locale::En, "changed")]),
    ("ui.preferences.no.commands", &[(Locale::En, "No commands to bind")]),
    ("ui.preferences.reassign.anyway", &[(Locale::En, "Reassign anyway")]),
    ("ui.preferences.keep.as.it.was", &[(Locale::En, "Keep as it was")]),
    ("ui.preferences.reset.all.shortcuts", &[(Locale::En, "Reset all shortcuts")]),
    ("ui.keymap.no.such.command", &[(Locale::En, "No such command")]),
    ("ui.keymap.already.used.by", &[(Locale::En, "Already used by")]),
    ("ui.docks.expand.the.dock", &[(Locale::En, "Expand the dock")]),
    ("ui.docks.character.face.none", &[(Locale::En, "No faces listed for this family name")]),
    ("ui.docks.character.family.not.installed", &[(Locale::En, "Not installed — shaping with")]),
    ("ui.docks.character.no.matching.family", &[(Locale::En, "No installed family matches")]),
    ("ui.docks.paragraph.boxed", &[(Locale::En, "Wrap to box")]),
    ("ui.docks.paragraph.box.height", &[(Locale::En, "Box height")]),
    ("ui.docks.paragraph.fixed.height", &[(Locale::En, "Fixed height")]),
    ("ui.docks.paragraph.overset", &[(Locale::En, "Text overflows the box")]),
    ("ui.docks.paragraph.overset.lines", &[(Locale::En, "lines past the box")]),
    ("ui.docks.close.panel", &[(Locale::En, "Close panel")]),
    ("ui.docks.move.this.panel", &[(Locale::En, "Move this panel")]),
    ("ui.docks.move.to", &[(Locale::En, "Move to")]),
    ("ui.docks.the.panel.is.already.on.this", &[(Locale::En, "The panel is already on this side")]),
    ("ui.docks.dock.this.panel.here", &[(Locale::En, "Dock this panel here")]),
    ("ui.docks.move.this.panel.within.its.dock", &[(Locale::En, "Move this panel within its dock")]),
    ("ui.docks.this.panel.is.already.at.the", &[(Locale::En, "This panel is already at the top of its dock")]),
    ("ui.docks.this.panel.is.already.at.the.2", &[(Locale::En, "This panel is already at the bottom of its dock")]),
    ("ui.docks.no.drop", &[(Locale::En, "no drop")]),
    ("ui.docks.mask.off", &[(Locale::En, "mask off")]),
    // Card 055: the two thumbnail wells of a layer row.
    (
        "ui.docks.content.thumbnail",
        &[(Locale::En, "content thumbnail — click to edit the layer's pixels")],
    ),
    (
        "ui.docks.mask.thumbnail",
        &[(Locale::En, "mask thumbnail — click to edit the layer's mask")],
    ),
    (
        "ui.docks.mask.target.badge",
        &[(
            Locale::En,
            "edits are aimed at this mask — right-click for view and mask options",
        )],
    ),
    ("ui.docks.mask.disable", &[(Locale::En, "Disable Mask")]),
    ("ui.docks.mask.enable", &[(Locale::En, "Enable Mask")]),
    ("ui.docks.mask.toggle.link", &[(Locale::En, "Toggle Link")]),
    ("ui.docks.mask.view.composite", &[(Locale::En, "Composite")]),
    ("ui.docks.mask.view.grayscale", &[(Locale::En, "Grayscale")]),
    ("ui.docks.mask.view.overlay", &[(Locale::En, "Overlay")]),
    (
        "ui.refine_mask.subtitle",
        &[(
            Locale::En,
            "Edge refinement: feather, shift, smooth, and contrast over the mask's existing coverage — not subject recognition.",
        )],
    ),
    ("ui.refine_mask.feather", &[(Locale::En, "Feather")]),
    ("ui.refine_mask.shift", &[(Locale::En, "Shift Edge")]),
    ("ui.refine_mask.smooth", &[(Locale::En, "Smooth")]),
    ("ui.refine_mask.contrast", &[(Locale::En, "Contrast")]),
    ("ui.refine_mask.background.label", &[(Locale::En, "Preview against")]),
    ("ui.refine_mask.background.black", &[(Locale::En, "Black")]),
    ("ui.refine_mask.background.white", &[(Locale::En, "White")]),
    ("ui.refine_mask.background.checker", &[(Locale::En, "Checkerboard")]),
    ("ui.refine_mask.confirm", &[(Locale::En, "Refine")]),
    ("ui.refine_mask.title", &[(Locale::En, "Refine Mask")]),
    (
        "ui.defringe.subtitle",
        &[(
            Locale::En,
            "Color fringe cleanup: pulls the boundary pixels' color toward the nearby interior ink — a separate edit from mask refinement.",
        )],
    ),
    ("ui.defringe.radius", &[(Locale::En, "Radius")]),
    ("ui.defringe.strength", &[(Locale::En, "Strength")]),
    ("ui.defringe.confirm", &[(Locale::En, "Remove Fringe")]),
    ("ui.defringe.title", &[(Locale::En, "Remove Color Fringe")]),
    (
        "ui.defringe.nothing.to.clean",
        &[(Locale::En, "Every parameter is at its neutral value — there is nothing to clean")],
    ),
    ("ui.refine_mask.px.suffix", &[(Locale::En, " px")]),
    (
        "ui.refine_mask.nothing.to.refine",
        &[(Locale::En, "Every parameter is at its neutral value — there is nothing to refine")],
    ),
    // W2-F: the About, Trim, New Guide and Rename Layer dialogs.
    ("ui.about.title", &[(Locale::En, "About Raster Studio")]),
    ("ui.about.tagline", &[(Locale::En, "A layered raster editor")]),
    ("ui.about.third.party.notices", &[(Locale::En, "Third-party notices")]),
    ("ui.trim.subtitle", &[(Locale::En, "Crops the canvas to the content, judged by the basis below.")]),
    ("ui.trim.based.on", &[(Locale::En, "Based On")]),
    ("ui.trim.transparent.pixels", &[(Locale::En, "Transparent Pixels")]),
    ("ui.trim.top.left.color", &[(Locale::En, "Top Left Pixel Color")]),
    ("ui.trim.bottom.right.color", &[(Locale::En, "Bottom Right Pixel Color")]),
    ("ui.trim.trim.away", &[(Locale::En, "Trim Away")]),
    ("ui.trim.choose.a.side", &[(Locale::En, "Choose at least one side to trim")]),
    ("ui.new_guide.title", &[(Locale::En, "New Guide")]),
    ("ui.new_guide.subtitle", &[(Locale::En, "Adds one guide at a document coordinate, in pixels.")]),
    ("ui.new_guide.position.must.be.finite", &[(Locale::En, "The position must be a finite number")]),
    ("ui.rename_layer.title", &[(Locale::En, "Rename Layer")]),
    ("ui.rename_layer.name.empty", &[(Locale::En, "The name cannot be empty")]),
    ("ui.rename_layer.name.unchanged", &[(Locale::En, "The name has not changed")]),
    ("ui.duplicate_layer.title", &[(Locale::En, "Duplicate Layer")]),
    ("ui.duplicate_layer.name.empty", &[(Locale::En, "The name cannot be empty")]),
    ("ui.docks.show.every.layer", &[(Locale::En, "Show every layer")]),
    ("ui.docks.thumbnail.size", &[(Locale::En, "Thumbnail size")]),
    ("ui.docks.link.selected.layers", &[(Locale::En, "Link selected layers")]),
    ("ui.docks.open.the.adjustments.panel", &[(Locale::En, "Open the Adjustments panel")]),
    ("ui.docks.blending.options", &[(Locale::En, "Blending options")]),
    ("ui.docks.select.a.layer.first", &[(Locale::En, "Select a layer first")]),
    ("ui.docks.add.a.layer.mask", &[(Locale::En, "Add a layer mask")]),
    ("ui.docks.new.layer", &[(Locale::En, "New layer")]),
    ("ui.docks.new.group", &[(Locale::En, "New group")]),
    ("ui.docks.delete.selected.layers", &[(Locale::En, "Delete selected layers")]),
    ("ui.docks.mark.this.state.so.you.can", &[(Locale::En, "Mark this state so you can come back to it")]),
    ("ui.docks.the.steps.this.snapshot.named.have", &[(Locale::En, "The steps this snapshot named have been discarded")]),
    ("ui.docks.add.an.adjustment.layer", &[(Locale::En, "Add an adjustment layer")]),
    ("ui.docks.select.a.layer.to.see.its", &[(Locale::En, "Select a layer to see its properties")]),
    ("ui.docks.type.is.edited.in.character.and", &[(Locale::En, "Type is edited in Character and Paragraph")]),
    ("ui.docks.path.editing.lives.in.the.paths", &[(Locale::En, "Path editing lives in the Paths panel")]),
    ("ui.docks.clip.to.layer.below", &[(Locale::En, "Clip to layer below")]),
    ("ui.docks.this.layer.has.no.mask", &[(Locale::En, "This layer has no mask")]),
    ("ui.docks.invert.coverage", &[(Locale::En, "Invert coverage")]),
    ("ui.docks.apply.this.mask", &[(Locale::En, "Apply this mask")]),
    ("ui.docks.move.with.the.layer", &[(Locale::En, "Move with the layer")]),
    ("ui.docks.this.adjustment.has.no.panel.controls", &[(Locale::En, "This adjustment has no panel controls")]),
    ("ui.docks.invert.has.no.parameters", &[(Locale::En, "Invert has no parameters")]),
    ("ui.docks.open.editor", &[(Locale::En, "Open editor…")]),
    ("ui.docks.sample.a.colour.from.the.canvas", &[(Locale::En, "Sample a colour from the canvas")]),
    ("ui.docks.out.of.gamut", &[(Locale::En, "Out of gamut")]),
    ("ui.docks.add.current.colour", &[(Locale::En, "Add current colour")]),
    ("ui.docks.right.click.a.swatch.to.remove", &[(Locale::En, "Right-click a swatch to remove it")]),
    ("ui.docks.edit.brush", &[(Locale::En, "Edit brush…")]),
    ("ui.docks.save.current.brush", &[(Locale::En, "Save current brush")]),
    ("ui.docks.auto.leading", &[(Locale::En, "Auto leading")]),
    ("ui.docks.align.pick", &[(Locale::En, "To canvas")]),
    ("ui.docks.properties.transform.toggle", &[(Locale::En, "Show or hide the position, size and alignment fields")]),
    ("ui.docks.align.left", &[(Locale::En, "Align the left edge to the canvas")]),
    ("ui.docks.align.hcenter", &[(Locale::En, "Center horizontally on the canvas")]),
    ("ui.docks.align.right", &[(Locale::En, "Align the right edge to the canvas")]),
    ("ui.docks.align.top", &[(Locale::En, "Align the top edge to the canvas")]),
    ("ui.docks.align.vcenter", &[(Locale::En, "Center vertically on the canvas")]),
    ("ui.docks.align.bottom", &[(Locale::En, "Align the bottom edge to the canvas")]),
    ("ui.docks.properties.nothing.to.measure", &[(Locale::En, "Nothing to measure yet — paint or place something on this layer first")]),
    ("ui.docks.properties.position.locked", &[(Locale::En, "Position is locked — unlock it in the Layers panel to move or resize")]),
    ("ui.docks.shape.filled", &[(Locale::En, "Paint the inside of the path")]),
    ("ui.docks.shape.stroked", &[(Locale::En, "Outline the path")]),
    ("ui.docks.shape.no.radius", &[(Locale::En, "Corner radius applies to rectangles; this path has no corners to round")]),
    ("ui.docks.shape.radius", &[(Locale::En, "Corner radius")]),
    ("ui.docks.smart.embedded", &[(Locale::En, "Embedded source")]),
    ("ui.docks.smart.linked", &[(Locale::En, "Linked file")]),
    ("ui.docks.smart.no.source", &[(Locale::En, "No source recorded for this object")]),
    ("ui.docks.layers.search", &[(Locale::En, "Search layers by name")]),
    ("ui.docks.layers.rename.tip", &[(Locale::En, "Double-click to rename")]),
    ("ui.docks.character.kerning", &[(Locale::En, "Pair kerning")]),
    ("ui.docks.character.kerning.tip", &[(Locale::En, "Metrics uses the font's own pair kerning; 0 turns kerning off; Manual puts one amount (1/1000 em) between every pair of the text as it is now, and characters typed later start unkerned; it needs at least two characters, so shorter text and the Type tool defaults do not offer it. The shaper has no optical kerning, so that mode is not offered.")]),
    ("ui.docks.character.kerning.amount", &[(Locale::En, "Amount")]),
    ("ui.docks.character.ligatures", &[(Locale::En, "Standard ligatures")]),
    ("ui.docks.character.script.tip", &[(Locale::En, "Superscript raises and shrinks the text; subscript lowers and shrinks it.")]),
    ("ui.docks.character.hscale", &[(Locale::En, "Scale H")]),
    ("ui.docks.character.vscale", &[(Locale::En, "Scale V")]),
    ("ui.docks.character.hscale.tip", &[(Locale::En, "Horizontal scale, in percent: widens or narrows the glyphs and their spacing")]),
    ("ui.docks.character.vscale.tip", &[(Locale::En, "Vertical scale, in percent: stretches the glyphs about the baseline; leading is unchanged")]),
    ("ui.docks.character.baseline.shift", &[(Locale::En, "Baseline shift")]),
    ("ui.docks.character.caps.tip", &[(Locale::En, "All Caps shapes every lowercase letter as its capital; Small Caps shapes it as a capital at 70 % size. The stored text keeps the case you typed.")]),
    ("ui.docks.character.antialias.tip", &[(Locale::En, "Smooth draws grey-scale edges; None draws hard, aliased edges. The glyph scaler has one smooth mode, so Sharp, Crisp and Strong are not offered.")]),
    ("ui.docks.character.type.defaults", &[(Locale::En, "Type tool defaults")]),
    ("ui.docks.character.type.defaults.note", &[(Locale::En, "The next text layer the Type tool creates starts with this style.")]),
    ("ui.docks.paragraph.last.line", &[(Locale::En, "Last line")]),
    ("ui.docks.paragraph.indent.left", &[(Locale::En, "Left indent")]),
    ("ui.docks.paragraph.indent.right", &[(Locale::En, "Right indent")]),
    ("ui.docks.paragraph.indent.first", &[(Locale::En, "First line")]),
    ("ui.docks.character.leading.tip", &[(Locale::En, "Baseline to baseline. Auto leading follows the type size.")]),
    ("ui.docks.zoom.out", &[(Locale::En, "Zoom out")]),
    ("ui.docks.zoom.in", &[(Locale::En, "Zoom in")]),
    ("ui.docks.fit.the.whole.image.in.the", &[(Locale::En, "Fit the whole image in the window")]),




    ("ui.layer_style.drop.shadow", &[(Locale::En, "Drop Shadow")]),
    ("ui.layer_style.inner.shadow", &[(Locale::En, "Inner Shadow")]),
    ("ui.layer_style.outer.glow", &[(Locale::En, "Outer Glow")]),
    ("ui.layer_style.inner.glow", &[(Locale::En, "Inner Glow")]),
    ("ui.layer_style.color.overlay", &[(Locale::En, "Color Overlay")]),
    ("ui.layer_style.gradient.overlay", &[(Locale::En, "Gradient Overlay")]),
    ("ui.layer_style.pattern.overlay", &[(Locale::En, "Pattern Overlay")]),
    ("ui.layer_style.effects.apply.to.the.whole.layer", &[(Locale::En, "Effects apply to the whole layer and undo as one step.")]),
    ("ui.layer_style.clear.all", &[(Locale::En, "Clear All")]),
    ("ui.layer_style.styles.enabled", &[(Locale::En, "Styles enabled")]),
    ("ui.layer_style.global.light", &[(Locale::En, "Global light")]),
    ("ui.layer_style.this.effect.is.off.tick.it", &[(Locale::En, "This effect is off. Tick it in the list to edit it.")]),
    ("ui.layer_style.align.with.layer", &[(Locale::En, "Align with layer")]),
    ("ui.layer_style.edit.this.ramp", &[(Locale::En, "Edit this ramp")]),
    ("ui.layer_style.click.the.ramp.to.edit.its", &[(Locale::En, "Click the ramp to edit its stops.")]),
    ("ui.layer_style.link.with.layer", &[(Locale::En, "Link with layer")]),
    ("ui.layer_style.approximate.the.composited.result.is.what", &[(Locale::En, "Approximate. The composited result is what the canvas shows.")]),
    ("ui.layer_style.use.global.light", &[(Locale::En, "Use global light")]),
    ("ui.layer_style.layer.knocks.out.drop.shadow", &[(Locale::En, "Layer knocks out drop shadow")]),
    ("ui.layer_style.layer.style", &[(Locale::En, "Layer Style")]),
    ("ui.layer_style.apply.style", &[(Locale::En, "Apply Style")]),
    // W2-X: the Blending Options page.
    ("ui.layer_style.blending.options", &[(Locale::En, "Blending Options")]),
    ("ui.layer_style.blending.mode", &[(Locale::En, "Mode")]),
    ("ui.layer_style.blending.opacity", &[(Locale::En, "Opacity")]),
    ("ui.layer_style.blending.fill", &[(Locale::En, "Fill")]),
    ("ui.layer_style.blending.caption", &[(Locale::En, "Opacity scales the layer and its effects; Fill scales the layer's own pixels only.")]),
    ("ui.export_as.give.the.export.a.file.name", &[(Locale::En, "Give the export a file name")]),
    ("ui.export_as.enable.at.least.one.export", &[(Locale::En, "Enable at least one export")]),
    ("ui.export_as.an.export.needs.at.least.one", &[(Locale::En, "An export needs at least one output")]),
    ("ui.export_as.every.enabled.row.is.written.when", &[(Locale::En, "Every enabled row is written when you export.")]),
    ("ui.export_as.file.name", &[(Locale::En, "File name")]),
    ("ui.export_as.live.preview", &[(Locale::En, "Live preview")]),
    ("ui.export_as.this.format.could.not.be.previewed", &[(Locale::En, "This format could not be previewed.")]),
    ("ui.export_as.live.preview.is.off", &[(Locale::En, "Live preview is off.")]),
    ("ui.export_as.add.export", &[(Locale::En, "Add export")]),
    ("ui.export_as.remove.export", &[(Locale::En, "Remove export")]),
    ("ui.export_as.no.export.selected", &[(Locale::En, "No export selected")]),
    ("ui.export_as.this.format.stores.8.bits.per", &[(Locale::En, "This format stores 8 bits per channel")]),
    ("ui.export_as.embed.colour.profile", &[(Locale::En, "Embed colour profile")]),
    ("ui.export_as.embed.exif.and.xmp", &[(Locale::En, "Embed EXIF and XMP")]),
    ("ui.export_as.webp.lossless", &[(Locale::En, "WebP (lossless)")]),
    ("ui.export_as.export.as", &[(Locale::En, "Export As")]),
    ("ui.brush_editor.custom.brush", &[(Locale::En, "Custom Brush")]),
    ("ui.brush_editor.the.preview.runs.the.real.brush", &[(Locale::En, "The preview runs the real brush engine.")]),
    ("ui.brush_editor.aliased.pencil", &[(Locale::En, "Aliased (pencil)")]),
    ("ui.brush_editor.pressure.controls.size", &[(Locale::En, "Pressure controls size")]),
    ("ui.brush_editor.pressure.controls.flow", &[(Locale::En, "Pressure controls flow")]),
    ("ui.brush_editor.min.size", &[(Locale::En, "Min size")]),
    ("ui.brush_editor.minimum.size.only.applies.when.pressure", &[(Locale::En, "Minimum size only applies when pressure controls size.")]),
    ("ui.brush_editor.brush.editor", &[(Locale::En, "Brush Editor")]),
    ("ui.brush_editor.save.brush", &[(Locale::En, "Save Brush")]),
    ("ui.brush_editor.give.the.brush.a.name", &[(Locale::En, "Give the brush a name")]),
    ("ui.gradient_editor.spectrum", &[(Locale::En, "Spectrum")]),
    ("ui.gradient_editor.sunset", &[(Locale::En, "Sunset")]),
    ("ui.gradient_editor.copper", &[(Locale::En, "Copper")]),
    ("ui.gradient_editor.black.to.white", &[(Locale::En, "Black to White")]),
    ("ui.gradient_editor.white.to.black", &[(Locale::En, "White to Black")]),
    ("ui.gradient_editor.black.to.transparent", &[(Locale::En, "Black to Transparent")]),
    ("ui.gradient_editor.opacity.stops.sit.above.the.bar", &[(Locale::En, "Opacity stops sit above the bar, colour stops below it.")]),
    ("ui.gradient_editor.no.stop.selected", &[(Locale::En, "No stop selected")]),
    ("ui.gradient_editor.the.last.stop.has.no.segment", &[(Locale::En, "The last stop has no segment after it.")]),
    ("ui.gradient_editor.delete.stop", &[(Locale::En, "Delete stop")]),
    ("ui.gradient_editor.gradient.editor", &[(Locale::En, "Gradient Editor")]),
    ("ui.image_size.change.how.many.pixels.the.document", &[(Locale::En, "Change how many pixels the document has, or how large it prints.")]),
    ("ui.image_size.pixel.dimensions", &[(Locale::En, "Pixel dimensions")]),
    ("ui.image_size.turn.on.resample.to.change.the", &[(Locale::En, "Turn on Resample to change the pixel count.")]),
    ("ui.image_size.document.size", &[(Locale::En, "Document size")]),
    ("ui.image_size.constrain.proportions", &[(Locale::En, "Constrain proportions")]),
    ("ui.image_size.nearest.neighbour", &[(Locale::En, "Nearest Neighbour")]),
    ("ui.image_size.soft.and.cheap.good.for.a", &[(Locale::En, "Soft and cheap. Good for a small enlargement.")]),
    ("ui.image_size.the.balanced.default.for.photographs", &[(Locale::En, "The balanced default for photographs.")]),
    ("ui.image_size.sharpest.with.a.little.ringing.on", &[(Locale::En, "Sharpest, with a little ringing on hard edges.")]),
    ("ui.image_size.image.size", &[(Locale::En, "Image Size")]),
    ("ui.image_size.width.and.height.must.be.at", &[(Locale::En, "Width and height must be at least 1 pixel")]),
    ("ui.image_size.resolution.must.be.greater.than.zero", &[(Locale::En, "Resolution must be greater than zero")]),
    ("ui.canvas_size.top.left", &[(Locale::En, "Top left")]),
    ("ui.canvas_size.top.right", &[(Locale::En, "Top right")]),
    ("ui.canvas_size.bottom.left", &[(Locale::En, "Bottom left")]),
    ("ui.canvas_size.bottom.right", &[(Locale::En, "Bottom right")]),
    ("ui.canvas_size.add.or.remove.room.around.the", &[(Locale::En, "Add or remove room around the image. Pixels are not resampled.")]),
    ("ui.canvas_size.new.size", &[(Locale::En, "New size")]),
    ("ui.canvas_size.canvas.extension", &[(Locale::En, "Canvas extension")]),
    ("ui.canvas_size.canvas.size", &[(Locale::En, "Canvas Size")]),
    ("ui.canvas_size.resize.canvas", &[(Locale::En, "Resize Canvas")]),
    ("ui.canvas_size.the.canvas.must.be.at.least", &[(Locale::En, "The canvas must be at least 1 x 1 pixel")]),
    ("ui.mod.lock.transparent.pixels", &[(Locale::En, "Lock transparent pixels")]),
    ("ui.mod.lock.pixels", &[(Locale::En, "Lock pixels")]),
    ("ui.mod.lock.position", &[(Locale::En, "Lock position")]),
    ("ui.mod.lock.all", &[(Locale::En, "Lock all")]),
    ("ui.menu_bar.nothing.in.this.submenu.is.available", &[(Locale::En, "Nothing in this submenu is available right now")]),
    ("ui.status.unsaved.changes", &[(Locale::En, "Unsaved changes")]),
    ("ui.status.type.a.zoom.level", &[(Locale::En, "Type a zoom level")]),
    // The application chrome (`app_shell::chrome`): the tab strip, the status
    // strip and the start screen.
    ("ui.chrome.no.document", &[(Locale::En, "No document")]),
    ("ui.chrome.not.saved.yet", &[(Locale::En, "Not saved yet")]),
    ("ui.chrome.close.tab", &[(Locale::En, "Close")]),
    ("ui.chrome.more.tabs", &[(Locale::En, "More tabs")]),
    ("ui.chrome.more.readouts", &[(Locale::En, "More readouts")]),
    ("ui.chrome.start.new", &[(Locale::En, "New")]),
    ("ui.chrome.start.new.hint", &[(Locale::En, "A blank canvas at any size")]),
    ("ui.chrome.start.open", &[(Locale::En, "Open\u{2026}")]),
    ("ui.chrome.start.open.hint", &[(Locale::En, "An image or project from this computer")]),
    ("ui.chrome.start.templates", &[(Locale::En, "Templates")]),
    ("ui.chrome.start.templates.hint", &[(Locale::En, "Screen, print and social presets")]),
    ("ui.chrome.start.recent", &[(Locale::En, "Recent")]),
    ("ui.chrome.start.no.recent", &[(Locale::En, "No recent files yet")]),
    ("ui.toolbar.swap.foreground.and.background.x", &[(Locale::En, "Swap foreground and background (X)")]),
    ("ui.toolbar.default.colours.d", &[(Locale::En, "Default colours (D)")]),
    ("ui.toolbar.this.tool.has.no.options", &[(Locale::En, "This tool has no options")]),
    ("ui.toolbar.this.tool.is.already.at.its", &[(Locale::En, "This tool is already at its defaults")]),
    ("ui.toolbar.return.this.tool.to.its.defaults", &[(Locale::En, "Return this tool to its defaults")]),
    ("ui.toolbar.swap.colours.x", &[(Locale::En, "Swap colours  (X)")]),
    ("ui.toolbar.default.colours.d.2", &[(Locale::En, "Default colours  (D)")]),
    ("ui.canvas_rotation.the.canvas.grows.to.fit.the", &[(Locale::En, "The canvas grows to fit the rotated image.")]),
    ("ui.canvas_rotation.rotate.canvas", &[(Locale::En, "Rotate Canvas")]),
    ("ui.canvas_rotation.the.angle.must.be.a.finite", &[(Locale::En, "The angle must be a finite number of degrees")]),
    ("ui.color_picker.this.window.cannot.read.screen.pixels", &[(Locale::En, "This window cannot read screen pixels, so the eyedropper is unavailable")]),
    ("ui.color_picker.back.to.the.colour.this.opened", &[(Locale::En, "Back to the colour this opened on")]),
    ("ui.color_picker.only.web.safe.colours", &[(Locale::En, "Only web-safe colours")]),
    ("ui.color_picker.click.anywhere.to.sample.a.colour", &[(Locale::En, "Click anywhere to sample a colour, or press Escape.")]),
    ("ui.color_picker.not.a.hex.colour", &[(Locale::En, "Not a hex colour")]),
    ("ui.color_picker.color.picker", &[(Locale::En, "Color Picker")]),
    ("ui.fill_stroke.no.patterns.are.defined.yet", &[(Locale::En, "No patterns are defined yet")]),
    ("ui.fill_stroke.width.must.be.between.1.and", &[(Locale::En, "Width must be between 1 and 250 pixels")]),
    ("ui.fill_stroke.fills.the.active.selection.with.the", &[(Locale::En, "Fills the active selection with the chosen contents.")]),
    ("ui.fill_stroke.preserve.transparency", &[(Locale::En, "Preserve Transparency")]),
    ("ui.fill_stroke.paints.a.band.along.the.active", &[(Locale::En, "Paints a band along the active selection's border.")]),
    ("ui.filter_gallery.filter.gallery", &[(Locale::En, "Filter Gallery")]),
    // W1-H: the Image > Adjustments dialog.
    ("ui.adjustment.confirm", &[(Locale::En, "Apply")]),
    ("ui.adjustment.reset", &[(Locale::En, "Reset")]),
    ("ui.adjustment.preview", &[(Locale::En, "Preview")]),
    ("ui.adjustment.preview.off", &[(Locale::En, "Preview is off.")]),
    ("ui.adjustment.nothing.to.preview", &[(Locale::En, "Nothing to preview.")]),
    ("ui.adjustment.subtitle", &[(Locale::En, "Previewed on the active layer; applied to its pixels as one undoable step.")]),
    ("ui.adjustment.blocked.identity", &[(Locale::En, "Move a control first: these settings would change nothing")]),
    ("ui.adjustment.no.settings", &[(Locale::En, "This adjustment has no settings.")]),
    ("ui.adjustment.histogram", &[(Locale::En, "Luminance of the preview source; the markers are the black and white points.")]),
    ("ui.adjustment.brightness", &[(Locale::En, "Brightness")]),
    ("ui.adjustment.contrast", &[(Locale::En, "Contrast")]),
    ("ui.adjustment.black", &[(Locale::En, "Black")]),
    ("ui.adjustment.white", &[(Locale::En, "White")]),
    ("ui.adjustment.gamma", &[(Locale::En, "Gamma")]),
    ("ui.adjustment.exposure", &[(Locale::En, "Exposure")]),
    ("ui.adjustment.vibrance", &[(Locale::En, "Vibrance")]),
    ("ui.adjustment.saturation", &[(Locale::En, "Saturation")]),
    ("ui.adjustment.hue", &[(Locale::En, "Hue")]),
    ("ui.adjustment.lightness", &[(Locale::En, "Lightness")]),
    ("ui.adjustment.levels", &[(Locale::En, "Levels")]),
    ("ui.adjustment.level", &[(Locale::En, "Level")]),
    ("ui.adjustment.tone", &[(Locale::En, "Tone")]),
    ("ui.adjustment.shadows", &[(Locale::En, "Shadows")]),
    ("ui.adjustment.midtones", &[(Locale::En, "Midtones")]),
    ("ui.adjustment.highlights", &[(Locale::En, "Highlights")]),
    ("ui.adjustment.cyan.red", &[(Locale::En, "Cyan / Red")]),
    ("ui.adjustment.magenta.green", &[(Locale::En, "Magenta / Green")]),
    ("ui.adjustment.yellow.blue", &[(Locale::En, "Yellow / Blue")]),
    ("ui.adjustment.preserve.luminosity", &[(Locale::En, "Preserve luminosity")]),
    ("ui.adjustment.reds", &[(Locale::En, "Reds")]),
    ("ui.adjustment.yellows", &[(Locale::En, "Yellows")]),
    ("ui.adjustment.greens", &[(Locale::En, "Greens")]),
    ("ui.adjustment.cyans", &[(Locale::En, "Cyans")]),
    ("ui.adjustment.blues", &[(Locale::En, "Blues")]),
    ("ui.adjustment.magentas", &[(Locale::En, "Magentas")]),
    ("ui.adjustment.whites", &[(Locale::En, "Whites")]),
    ("ui.adjustment.neutrals", &[(Locale::En, "Neutrals")]),
    ("ui.adjustment.blacks", &[(Locale::En, "Blacks")]),
    ("ui.adjustment.tint", &[(Locale::En, "Tint")]),
    ("ui.adjustment.tint.hue", &[(Locale::En, "Tint hue")]),
    ("ui.adjustment.tint.saturation", &[(Locale::En, "Tint saturation")]),
    ("ui.adjustment.color", &[(Locale::En, "Color")]),
    ("ui.adjustment.density", &[(Locale::En, "Density")]),
    ("ui.adjustment.output.channel", &[(Locale::En, "Output channel")]),
    ("ui.adjustment.red", &[(Locale::En, "Red")]),
    ("ui.adjustment.green", &[(Locale::En, "Green")]),
    ("ui.adjustment.blue", &[(Locale::En, "Blue")]),
    ("ui.adjustment.constant", &[(Locale::En, "Constant")]),
    ("ui.adjustment.monochrome", &[(Locale::En, "Monochrome")]),
    ("ui.adjustment.reverse", &[(Locale::En, "Reverse")]),
    ("ui.adjustment.stop", &[(Locale::En, "Stop")]),
    ("ui.adjustment.colors", &[(Locale::En, "Colors")]),
    ("ui.adjustment.cyan", &[(Locale::En, "Cyan")]),
    ("ui.adjustment.magenta", &[(Locale::En, "Magenta")]),
    ("ui.adjustment.yellow", &[(Locale::En, "Yellow")]),
    ("ui.adjustment.black.ink", &[(Locale::En, "Black")]),
    ("ui.adjustment.relative", &[(Locale::En, "Relative")]),
    ("ui.adjustment.curve.0", &[(Locale::En, "Shadows")]),
    ("ui.adjustment.curve.1", &[(Locale::En, "Quarter tones")]),
    ("ui.adjustment.curve.2", &[(Locale::En, "Midtones")]),
    ("ui.adjustment.curve.3", &[(Locale::En, "Three-quarter tones")]),
    ("ui.adjustment.curve.4", &[(Locale::En, "Highlights")]),
    // W2-D: the second right column, the Histogram panel, the Navigator's
    // zoom slider, the Info sample and the Channels footer.
    ("ui.docks.side.left", &[(Locale::En, "Left")]),
    ("ui.docks.side.narrow", &[(Locale::En, "Right (narrow)")]),
    ("ui.docks.side.right", &[(Locale::En, "Right (wide)")]),
    ("ui.docks.side.bottom", &[(Locale::En, "Bottom")]),
    ("ui.docks.histogram.no.composite", &[(Locale::En, "Open a document to see its histogram")]),
    ("ui.docks.histogram.waiting", &[(Locale::En, "Waiting for the composite…")]),
    ("ui.docks.histogram.empty", &[(Locale::En, "The image has no opaque pixels to count")]),
    ("ui.docks.histogram.rgb", &[(Locale::En, "RGB")]),
    ("ui.docks.histogram.luminosity", &[(Locale::En, "Luminosity")]),
    ("ui.docks.histogram.mean", &[(Locale::En, "Mean")]),
    ("ui.docks.histogram.pixels", &[(Locale::En, "Pixels")]),
    ("ui.docks.zoom.slider", &[(Locale::En, "Drag to zoom the view")]),
    ("ui.docks.channels.thumbnail", &[(Locale::En, "Channel thumbnail")]),
    ("ui.docks.channels.load.selection", &[(Locale::En, "Load channel as selection")]),
    ("ui.docks.channels.save.selection", &[(Locale::En, "Save selection as channel")]),
    ("ui.docks.channels.new", &[(Locale::En, "New channel")]),
    ("ui.docks.channels.delete", &[(Locale::En, "Delete channel")]),
    ("ui.docks.channels.no.mask.route", &[(Locale::En, "No command loads a channel as the selection in this build; the Select menu's Load Selection restores a saved selection instead")]),
    ("ui.docks.channels.no.selection", &[(Locale::En, "Make a selection first")]),
    ("ui.docks.channels.no.alpha.store", &[(Locale::En, "This build keeps channels on layers; it has no free-standing alpha channels yet")]),
    ("ui.docks.channels.not.a.mask", &[(Locale::En, "Only a mask channel can be loaded as a selection here")]),
    ("ui.docks.channels.no.document", &[(Locale::En, "No document is open")]),
    ("ui.docks.channels.saved.hint", &[(Locale::En, "A saved selection: click to open Select > Load Selection, then choose it by name there")]),
    ("actions.stop", &[(Locale::En, "Stop")]),
    ("actions.replay", &[(Locale::En, "Replay")]),
    (
        "actions.hint",
        &[(
            Locale::En,
            "Record an edit, then replay the whole sequence on any document with at least as many layers.",
        )],
    ),
];

/// Keys that must resolve. The tests walk this list, so a table row whose key
/// drifted is caught next to the constant that drifted. Test-only today: the
/// moment a second locale lands, the preferences UI reads this list too.
#[cfg(test)]
const KNOWN_KEYS: &[&str] = &[
    // W3-H: Color Range, Select ▸ Modify, Save / Load Selection.
    "ui.selection_modify.border.title",
    "ui.selection_modify.smooth.title",
    "ui.selection_modify.expand.title",
    "ui.selection_modify.contract.title",
    "ui.selection_modify.feather.title",
    "ui.selection_modify.width",
    "ui.selection_modify.sample.radius",
    "ui.selection_modify.expand.by",
    "ui.selection_modify.contract.by",
    "ui.selection_modify.feather.radius",
    "ui.selection_modify.apply",
    "ui.selection_modify.px",
    "ui.selection_modify.out.of.range",
    "ui.selection_modify.range.to",
    "ui.selection_modify.border.caption",
    "ui.selection_name.alpha",
    "ui.selection_name.save.title",
    "ui.selection_name.load.title",
    "ui.selection_name.empty",
    "ui.selection_name.taken",
    "ui.selection_name.name",
    "ui.selection_name.save.caption",
    "ui.selection_name.save",
    "ui.selection_name.op.new",
    "ui.selection_name.op.add",
    "ui.selection_name.op.subtract",
    "ui.selection_name.op.intersect",
    "ui.selection_name.op.needs.selection",
    "ui.selection_name.none.saved",
    "ui.selection_name.channel",
    "ui.selection_name.operation",
    "ui.selection_name.invert",
    "ui.selection_name.load",
    "ui.color_range.title",
    "ui.color_range.subtitle",
    "ui.color_range.view.selection",
    "ui.color_range.view.image",
    "ui.color_range.sampled.colour",
    "ui.color_range.fuzziness",
    "ui.color_range.invert",
    "ui.color_range.click.to.sample",
    "ui.color_range.click.preview",
    "ui.color_range.select",
    "ui.color_range.eyedropper",
    // W3-G: the Preferences dialog's live controls and the keymap editor.
    "ui.preferences.minutes.0.is.off",
    "ui.preferences.ui.scale",
    "ui.preferences.language",
    "ui.preferences.only.english",
    "ui.preferences.units",
    "ui.preferences.units.caption",
    "ui.preferences.scroll.wheel.zooms.instead.of.scrolling",
    "ui.preferences.scroll.wheel.caption",
    "ui.preferences.scratch.directory",
    "ui.preferences.scratch.caption",
    "ui.preferences.press.a.key",
    "ui.preferences.add.shortcut",
    "ui.preferences.remove.this.shortcut",
    "ui.preferences.changed",
    "ui.preferences.no.commands",
    "ui.preferences.reassign.anyway",
    "ui.preferences.keep.as.it.was",
    "ui.preferences.reset.all.shortcuts",
    "ui.keymap.no.such.command",
    "ui.keymap.already.used.by",
    // W2-F: About, Trim, New Guide, Rename Layer.
    "ui.about.title",
    "ui.about.tagline",
    "ui.about.third.party.notices",
    "ui.trim.subtitle",
    "ui.trim.based.on",
    "ui.trim.transparent.pixels",
    "ui.trim.top.left.color",
    "ui.trim.bottom.right.color",
    "ui.trim.trim.away",
    "ui.trim.choose.a.side",
    "ui.new_guide.title",
    "ui.new_guide.subtitle",
    "ui.new_guide.position.must.be.finite",
    "ui.rename_layer.title",
    "ui.rename_layer.name.empty",
    "ui.rename_layer.name.unchanged",
    "ui.duplicate_layer.title",
    "ui.duplicate_layer.name.empty",
    // W2-D: the narrow column, the Histogram, the Navigator slider, Channels.
    "ui.docks.side.left",
    "ui.docks.side.narrow",
    "ui.docks.side.right",
    "ui.docks.side.bottom",
    "ui.docks.histogram.no.composite",
    "ui.docks.histogram.waiting",
    "ui.docks.histogram.empty",
    "ui.docks.histogram.rgb",
    "ui.docks.histogram.luminosity",
    "ui.docks.histogram.mean",
    "ui.docks.histogram.pixels",
    "ui.docks.zoom.slider",
    "ui.docks.channels.thumbnail",
    "ui.docks.channels.load.selection",
    "ui.docks.channels.save.selection",
    "ui.docks.channels.new",
    "ui.docks.channels.delete",
    "ui.docks.channels.no.mask.route",
    "ui.docks.channels.no.selection",
    "ui.docks.channels.no.alpha.store",
    "ui.docks.channels.not.a.mask",
    "ui.docks.channels.no.document",
    "ui.docks.channels.saved.hint",
    "actions.record",
    "actions.stop",
    "actions.replay",
    "actions.hint",
    // Card 059: the mask well's popup rows and badge tooltip.
    "ui.docks.mask.target.badge",
    "ui.docks.mask.disable",
    "ui.docks.mask.enable",
    "ui.docks.mask.toggle.link",
    "ui.docks.mask.view.composite",
    "ui.docks.mask.view.grayscale",
    "ui.docks.mask.view.overlay",
    // Card 060: the Refine Mask dialog.
    "ui.refine_mask.subtitle",
    "ui.refine_mask.feather",
    "ui.refine_mask.shift",
    "ui.refine_mask.smooth",
    "ui.refine_mask.contrast",
    "ui.refine_mask.background.label",
    "ui.refine_mask.background.black",
    "ui.refine_mask.background.white",
    "ui.refine_mask.background.checker",
    "ui.refine_mask.confirm",
    "ui.refine_mask.title",
    "ui.defringe.subtitle",
    "ui.defringe.radius",
    "ui.defringe.strength",
    "ui.defringe.confirm",
    "ui.defringe.title",
    "ui.defringe.nothing.to.clean",
    "ui.refine_mask.px.suffix",
    "ui.refine_mask.nothing.to.refine",
    // W1-H: the Image > Adjustments dialog.
    "ui.adjustment.confirm",
    "ui.adjustment.reset",
    "ui.adjustment.preview",
    "ui.adjustment.preview.off",
    "ui.adjustment.nothing.to.preview",
    "ui.adjustment.subtitle",
    "ui.adjustment.blocked.identity",
    "ui.adjustment.no.settings",
    "ui.adjustment.histogram",
    "ui.adjustment.brightness",
    "ui.adjustment.contrast",
    "ui.adjustment.black",
    "ui.adjustment.white",
    "ui.adjustment.gamma",
    "ui.adjustment.exposure",
    "ui.adjustment.vibrance",
    "ui.adjustment.saturation",
    "ui.adjustment.hue",
    "ui.adjustment.lightness",
    "ui.adjustment.levels",
    "ui.adjustment.level",
    "ui.adjustment.tone",
    "ui.adjustment.shadows",
    "ui.adjustment.midtones",
    "ui.adjustment.highlights",
    "ui.adjustment.cyan.red",
    "ui.adjustment.magenta.green",
    "ui.adjustment.yellow.blue",
    "ui.adjustment.preserve.luminosity",
    "ui.adjustment.reds",
    "ui.adjustment.yellows",
    "ui.adjustment.greens",
    "ui.adjustment.cyans",
    "ui.adjustment.blues",
    "ui.adjustment.magentas",
    "ui.adjustment.whites",
    "ui.adjustment.neutrals",
    "ui.adjustment.blacks",
    "ui.adjustment.tint",
    "ui.adjustment.tint.hue",
    "ui.adjustment.tint.saturation",
    "ui.adjustment.color",
    "ui.adjustment.density",
    "ui.adjustment.output.channel",
    "ui.adjustment.red",
    "ui.adjustment.green",
    "ui.adjustment.blue",
    "ui.adjustment.constant",
    "ui.adjustment.monochrome",
    "ui.adjustment.reverse",
    "ui.adjustment.stop",
    "ui.adjustment.colors",
    "ui.adjustment.cyan",
    "ui.adjustment.magenta",
    "ui.adjustment.yellow",
    "ui.adjustment.black.ink",
    "ui.adjustment.relative",
    "ui.adjustment.curve.0",
    "ui.adjustment.curve.1",
    "ui.adjustment.curve.2",
    "ui.adjustment.curve.3",
    "ui.adjustment.curve.4",
    // W2-X: the Layer Style dialog's Blending Options page.
    "ui.layer_style.blending.options",
    "ui.layer_style.blending.mode",
    "ui.layer_style.blending.opacity",
    "ui.layer_style.blending.fill",
    "ui.layer_style.blending.caption",
    // W3-J: Properties transform / shape / smart-object pages, Layers search
    // and rename, Character kerning / ligatures / script.
    "ui.docks.align.pick",
    "ui.docks.properties.transform.toggle",
    "ui.docks.align.left",
    "ui.docks.align.hcenter",
    "ui.docks.align.right",
    "ui.docks.align.top",
    "ui.docks.align.vcenter",
    "ui.docks.align.bottom",
    "ui.docks.properties.nothing.to.measure",
    "ui.docks.properties.position.locked",
    "ui.docks.shape.filled",
    "ui.docks.shape.stroked",
    "ui.docks.shape.no.radius",
    "ui.docks.shape.radius",
    "ui.docks.smart.embedded",
    "ui.docks.smart.linked",
    "ui.docks.smart.no.source",
    "ui.docks.layers.search",
    "ui.docks.layers.rename.tip",
    "ui.docks.character.kerning",
    "ui.docks.character.kerning.tip",
    "ui.docks.character.kerning.amount",
    "ui.docks.character.ligatures",
    "ui.docks.character.script.tip",
    "ui.docks.character.hscale",
    "ui.docks.character.vscale",
    "ui.docks.character.hscale.tip",
    "ui.docks.character.vscale.tip",
    "ui.docks.character.baseline.shift",
    "ui.docks.character.caps.tip",
    "ui.docks.character.antialias.tip",
    "ui.docks.character.type.defaults",
    "ui.docks.character.type.defaults.note",
    "ui.docks.paragraph.last.line",
    "ui.docks.paragraph.indent.left",
    "ui.docks.paragraph.indent.right",
    "ui.docks.paragraph.indent.first",
    "ui.docks.character.leading.tip",
];

/// Resolve `key` in the active locale, falling back to English. An
/// unregistered key is a bug the catalogue tests catch; the tests name every
/// key the migrated modules use (KNOWN_KEYS), so a leak here means a module
/// grew a string without a table row. At runtime the empty string is better
/// than a panic or a rogue key leaking into the UI.
pub fn tr(key: &str) -> &'static str {
    let locale = active();
    let Some((_, row)) = TABLE.iter().find(|(k, _)| *k == key) else {
        return "";
    };
    row.iter()
        .find(|(l, _)| *l == locale)
        .or_else(|| row.iter().find(|(l, _)| *l == Locale::En))
        .map(|(_, s)| *s)
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_resolves_every_registered_key_for_every_locale() {
        for (key, row) in TABLE {
            for (locale, expected) in row.iter() {
                set_locale(*locale);
                assert_eq!(tr(key), *expected, "key {key:?} in {locale:?}");
            }
        }
        // English is always complete and always the fallback.
        set_locale(Locale::En);
        for (key, row) in TABLE {
            assert!(
                row.iter().any(|(l, _)| *l == Locale::En),
                "{key:?} has no English entry; English is the fallback source"
            );
        }
    }

    #[test]
    fn every_listed_locale_round_trips_through_its_code_and_unknown_codes_fall_back() {
        for locale in Locale::ALL {
            assert_eq!(Locale::from_code(locale.code()), *locale);
            set_locale(*locale);
            assert_eq!(active(), *locale);
        }
        assert_eq!(Locale::from_code("xx-not-a-locale"), Locale::En);
        set_locale(Locale::En);
    }

    #[test]
    fn an_unknown_key_is_empty_rather_than_a_leak_or_a_panic() {
        assert_eq!(tr("not.a.key"), "");
    }

    #[test]
    fn every_known_key_resolves_without_a_leak() {
        for key in KNOWN_KEYS {
            assert_ne!(tr(key), "", "{key} must resolve, not leak");
            assert!(
                TABLE.iter().any(|(k, _)| k == key),
                "{key} is in KNOWN_KEYS but not in TABLE"
            );
        }
    }
}
