import json
import re

MASK_PREFIX = "MASK"
MASK_SUFFIX = "/"

def slug(text):
    words = re.sub(r"[^A-Za-z0-9 ]", " ", text.lower()).split()
    return ".".join(words[:6])[:60].rstrip(".") or "text"

def esc(s):
    return s.replace("\\", "\\\\").replace('"', '\\"')

manifest = [
    ('crates/ui/src/view/menu_bar.rs', 'menu_bar', ['Nothing in this submenu is available right now']),
    ('crates/ui/src/view/status.rs', 'status', ['Unsaved changes', 'Type a zoom level']),
    ('crates/ui/src/view/toolbar.rs', 'toolbar', ['Swap foreground and background (X)', 'Default colours (D)', 'This tool has no options', 'This tool is already at its defaults', 'Return this tool to its defaults', 'Swap colours  (X)', 'Default colours  (D)']),
    ('crates/ui/src/dialogs/canvas_rotation.rs', 'canvas_rotation', ['The canvas grows to fit the rotated image.', 'Rotate Canvas', 'The angle must be a finite number of degrees']),
    ('crates/ui/src/dialogs/color_picker.rs', 'color_picker', ['This window cannot read screen pixels, so the eyedropper is unavailable', 'Back to the colour this opened on', 'Only web-safe colours', 'Click anywhere to sample a colour, or press Escape.', 'Not a hex colour', 'Color Picker']),
    ('crates/ui/src/dialogs/fill_stroke.rs', 'fill_stroke', ['No patterns are defined yet', 'Width must be between 1 and 250 pixels', 'Fills the active selection with the chosen contents.', 'Preserve Transparency', "Paints a band along the active selection's border."]),
    ('crates/ui/src/dialogs/filter_gallery.rs', 'filter_gallery', ['Filter Gallery']),
]

rows = []
used = set()
existing = open('crates/ui/src/strings.rs', encoding='utf-8').read()
manifest += [
    ('crates/ui/src/dialogs/image_size.rs', 'image_size', json.load(open('/tmp/i18n_b2.json'))['image_size']),
    ('crates/ui/src/dialogs/gradient_editor.rs', 'gradient_editor', json.load(open('/tmp/i18n_b2.json'))['gradient_editor']),
    ('crates/ui/src/dialogs/canvas_size.rs', 'canvas_size', json.load(open('/tmp/i18n_b2.json'))['canvas_size']),
    ('crates/ui/src/view/mod.rs', 'mod', json.load(open('/tmp/i18n_b2.json'))['mod']),
    ('crates/ui/src/dialogs/brush_editor.rs', 'brush_editor', json.load(open('/tmp/i18n_b2.json'))['brush_editor']),
    ('crates/ui/src/dialogs/export_as.rs', 'export_as', json.load(open('/tmp/i18n_b2.json'))['export_as']),
    ('crates/ui/src/dialogs/layer_style.rs', 'layer_style', json.load(open('/tmp/i18n_b2.json'))['layer_style']),
    ('crates/ui/src/dialogs/new_document.rs', 'new_document', json.load(open('/tmp/i18n_b2.json'))['new_document']),
    ('crates/ui/src/dialogs/preferences.rs', 'preferences', json.load(open('/tmp/i18n_b2.json'))['preferences']),
    ]

import sys
ONLY = sys.argv[1:] if len(sys.argv) > 1 else None
manifest = [m for m in manifest if (ONLY is None or m[1] in ONLY) and all(
    ('("%s"' % ('ui.' + m[1] + '.' + slug(l))) not in existing for l in m[2]
)]
for path, mod, lits in manifest:
    src = open(path, encoding='utf-8').read()
    cut = src.find('#[cfg(test)]')
    code, tests = (src[:cut], src[cut:]) if cut >= 0 else (src, '')
    # Mask the constructs a fn call cannot live in: const initialisers and
    # write!-macro format strings. Their literals stay for a hand edit.
    for lit in lits:
        key = 'ui.' + mod + '.' + slug(lit)
        base, n = key, 2
        while key in used:
            key = base + '.' + str(n)
            n += 1
        used.add(key)
        rows.append((key, lit, path))
        code = code.replace('"%s"' % lit, 'crate::strings::tr("%s")' % key)
    code = code.replace(MASK_PREFIX, '').replace(MASK_SUFFIX, '')
    if 'crate::strings::tr(' in code and 'use crate::strings::tr;' not in code:
        m = list(re.finditer(r'^use [^\n]+;$', code, flags=re.M))
        if m:
            pos = m[-1].end()
            code = code[:pos] + '\nuse crate::strings::tr;' + code[pos:]
    open(path, 'w', encoding='utf-8', newline='').write(code + tests)

t = open('crates/ui/src/strings.rs', encoding='utf-8').read()
anchor = '    ("actions.record", &[(Locale::En, "Record")]),'
new = '\n'.join('    ("%s", &[(Locale::En, "%s")]),' % (k, esc(l)) for k, l, _ in rows)
t = t.replace(anchor, anchor + '\n' + new, 1)
open('crates/ui/src/strings.rs', 'w', encoding='utf-8', newline='').write(t)
print('migrated', len(rows), 'literals across', len(manifest), 'files')

