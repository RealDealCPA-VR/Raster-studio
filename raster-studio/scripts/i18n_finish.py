"""Finish the P3.12 sweep: hand-listed literals that survived the batches."""
import re


def esc(s):
    return s.replace("\\", "\\\\").replace('"', '\\"')


jobs = [
    ('crates/ui/src/view/docks.rs', 'ui.docks.enter.a.colour', 'Enter a colour like #3366CC'),
    ('crates/ui/src/view/docks.rs', 'ui.docks.no.layers.yet', 'No layers yet. Add one with the + button below.'),
    ('crates/ui/src/view/docks.rs', 'ui.docks.show.hide.layer', 'Show / hide layer'),
    ('crates/ui/src/view/docks.rs', 'ui.docks.show.hide.channel', 'Show / hide this channel'),
    ('crates/ui/src/view/docks.rs', 'ui.docks.show.hide.path', 'Show / hide this path'),
    ('crates/ui/src/view/toolbar.rs', 'ui.toolbar.background.picker', 'Background — double-click for the picker'),
    ('crates/ui/src/view/toolbar.rs', 'ui.toolbar.gradient.stops', 'Edit gradient stops — click to open the editor'),
    ('crates/ui/src/view/toolbar.rs', 'ui.toolbar.foreground.picker', 'Foreground — double-click for the picker'),
    ('crates/ui/src/dialogs/canvas_rotation.rs', 'ui.canvas_rotation.rotates.everything', 'Rotates the canvas and every layer. Right angles are pixel-exact; other angles resample.'),
    ('crates/ui/src/dialogs/canvas_size.rs', 'ui.canvas_size.smaller.clips', 'The new canvas is smaller — content outside it will be clipped.'),
    ('crates/ui/src/dialogs/color_picker.rs', 'ui.color_picker.before.after', 'before / after'),
    ('crates/ui/src/dialogs/export_as.rs', 'ui.export_as.16.bit', '16 bit'),
    ('crates/ui/src/dialogs/export_as.rs', 'ui.export_as.8.bit', '8 bit'),
    ('crates/ui/src/dialogs/export_as.rs', 'ui.export_as.exif.not.implemented', 'EXIF and XMP writing is not implemented — only ICC is embedded'),
    ('crates/ui/src/dialogs/fill_stroke.rs', 'ui.fill_stroke.50.grey', '50% Grey'),
    ('crates/ui/src/dialogs/fill_stroke.rs', 'ui.fill_stroke.opacity.range', 'Opacity must be between 0% and 100%'),
    ('crates/ui/src/dialogs/filter_gallery.rs', 'ui.filter_gallery.pick.a.filter', 'Pick a filter; it applies at its default settings.'),
    ('crates/ui/src/dialogs/image_size.rs', 'ui.image_size.hard.edges', 'Hard edges, no blending. Pixel art only — it aliases on downscale.'),
    ('crates/ui/src/dialogs/layer_style.rs', 'ui.layer_style.bevel.emboss', 'Bevel & Emboss'),
    ('crates/ui/src/dialogs/layer_style.rs', 'ui.layer_style.no.pattern', 'No pattern chosen — the overlay paints nothing.'),
    ('crates/ui/src/dialogs/units.rs', 'ui.units.0.bytes', '0 bytes'),
]

pattern = re.compile('"((?:[^"\\\\]|\\\\.)*)"')
rows = []
for path, key, lit in jobs:
    src = open(path, encoding='utf-8').read()
    cut = src.find('#[cfg(test)]')
    code, tests = (src[:cut], src[cut:]) if cut >= 0 else (src, '')
    target = None
    for m in pattern.finditer(code):
        if re.sub(r'\s+', ' ', m.group(1)).strip() == lit.strip():
            target = m.group(1)
            break
    if target is None:
        print('MISS:', path, key)
        continue
    code = code.replace('"%s"' % target, 'crate::strings::tr("%s")' % key, 1)
    if 'crate::strings::tr(' in code and 'use crate::strings::tr;' not in code:
        m = list(re.finditer(r'^use [^\n]+;$', code, flags=re.M))
        if m:
            pos = m[-1].end()
            code = code[:pos] + '\nuse crate::strings::tr;' + code[pos:]
    open(path, 'w', encoding='utf-8', newline='').write(code + tests)
    rows.append((key, re.sub(r'\s+', ' ', lit).strip()))

t = open('crates/ui/src/strings.rs', encoding='utf-8').read()
anchor = '    ("actions.record", &[(Locale::En, "Record")]),'
fresh = [(k, l) for k, l in rows if ('("%s"' % k) not in t]
new = '\n'.join('    ("%s", &[(Locale::En, "%s")]),' % (k, esc(l)) for k, l in fresh)
t = t.replace(anchor, anchor + '\n' + new, 1)
open('crates/ui/src/strings.rs', 'w', encoding='utf-8', newline='').write(t)
print('rows added:', len(fresh), 'misses:', len(jobs) - len(rows))
