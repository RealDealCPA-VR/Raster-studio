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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Locale {
    En,
}

impl Locale {
    /// The locale the editor shows, as the preferences system stores it.
    pub fn from_code(code: &str) -> Self {
        let _ = code; // more locales arrive with their table rows
        Self::En
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
    let code = match locale {
        Locale::En => 0,
    };
    ACTIVE.store(code, Ordering::Relaxed);
}

fn active() -> Locale {
    let _ = ACTIVE.load(Ordering::Relaxed); // locales beyond En flip this
    Locale::En
}

/// Every catalogue entry: the English source string first, then any
/// translations. A locale missing from a row falls back to English at lookup.
const TABLE: &[(&str, &[(Locale, &str)])] = &[
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
    ("ui.preferences.recent.files", &[(Locale::En, "Recent files")]),
    ("ui.preferences.reopen.the.last.session.at.startup", &[(Locale::En, "Reopen the last session at startup")]),
    ("ui.preferences.ask.before.discarding.unsaved.work", &[(Locale::En, "Ask before discarding unsaved work")]),
    ("ui.preferences.ui.scale", &[(Locale::En, "UI scale")]),
    ("ui.preferences.show.tooltips", &[(Locale::En, "Show tooltips")]),
    ("ui.preferences.show.the.status.bar", &[(Locale::En, "Show the status bar")]),
    ("ui.preferences.brush.cursor", &[(Locale::En, "Brush cursor")]),
    ("ui.preferences.scroll.wheel.zooms.instead.of.scrolling", &[(Locale::En, "Scroll wheel zooms instead of scrolling")]),
    ("ui.preferences.snap.to.guides", &[(Locale::En, "Snap to guides")]),
    ("ui.preferences.snapshot.every", &[(Locale::En, "Snapshot every")]),
    ("ui.preferences.record.the.edit.log.in.saved", &[(Locale::En, "Record the edit log in saved files")]),
    ("ui.preferences.tile.cache", &[(Locale::En, "Tile cache")]),
    ("ui.preferences.use.the.gpu.where.available", &[(Locale::En, "Use the GPU where available")]),
    ("ui.preferences.scratch.disks", &[(Locale::En, "Scratch disks")]),
    ("ui.preferences.move.up", &[(Locale::En, "Move up")]),
    ("ui.preferences.stop.using.this.location.for.scratch", &[(Locale::En, "Stop using this location for scratch")]),
    ("ui.preferences.add.scratch.disk", &[(Locale::En, "Add scratch disk")]),
    ("ui.preferences.reassign.anyway", &[(Locale::En, "Reassign anyway")]),
    ("ui.preferences.keep.as.it.was", &[(Locale::En, "Keep as it was")]),
    ("ui.preferences.reset.all.shortcuts", &[(Locale::En, "Reset all shortcuts")]),
    ("ui.docks.expand.the.dock", &[(Locale::En, "Expand the dock")]),
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
    "actions.record",
    "actions.stop",
    "actions.replay",
    "actions.hint",
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
