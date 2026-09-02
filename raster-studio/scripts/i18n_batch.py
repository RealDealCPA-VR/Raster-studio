"""One i18n migration batch: migrate the named module(s) or all unmigrated ones.

Per the ledger plan: NEVER a blind cross-file batch without a compile check.
Usage:  python scripts/i18n_batch.py [module ...]   (FORCE=1 to re-run)
"""
import json
import os
import re
import sys

MASK_PREFIX = "\x01MASK\x01"
MASK_SUFFIX = "\x01/MASK\x01"


def slug(text):
    words = re.sub(r"[^A-Za-z0-9 ]", " ", text.lower()).split()
    return ".".join(words[:6])[:60].rstrip(".") or "text"


def esc(s):
    return s.replace("\\", "\\\\").replace('"', '\\"')


FORCE = os.environ.get("FORCE") == "1"
ONLY = sys.argv[1:] if len(sys.argv) > 1 else None
MANIFEST = json.load(open("scripts/i18n_manifest.json", encoding="utf-8"))

existing = open("crates/ui/src/strings.rs", encoding="utf-8").read()


def not_yet(entry):
    if FORCE:
        return True
    return all(
        ('("%s"' % ("ui." + entry["module"] + "." + slug(l))) not in existing
        for l in entry["literals"]
    )


manifest = [m for m in MANIFEST if (ONLY is None or m["module"] in ONLY) and not_yet(m)]
print("batch:", [(m["module"], len(m["literals"])) for m in manifest])

rows = []
used = set()
for path, mod, lits in [(m["file"], m["module"], m["literals"]) for m in manifest]:
    src = open(path, encoding="utf-8").read()
    cut = src.find("#[cfg(test)]")
    code, tests = (src[:cut], src[cut:]) if cut >= 0 else (src, "")
    for lit in lits:
        key = "ui." + mod + "." + slug(lit)
        base, n = key, 2
        while key in used:
            key = base + "." + str(n)
            n += 1
        used.add(key)
        rows.append((key, lit, path))
        code = code.replace('"%s"' % lit, 'crate::strings::tr("%s")' % key)
    if 'crate::strings::tr(' in code and 'use crate::strings::tr;' not in code:
        m = list(re.finditer(r"^use [^\n]+;$", code, flags=re.M))
        if m:
            pos = m[-1].end()
            code = code[:pos] + "\nuse crate::strings::tr;" + code[pos:]
    open(path, "w", encoding="utf-8", newline="").write(code + tests)

t = open("crates/ui/src/strings.rs", encoding="utf-8").read()
anchor = '    ("actions.record", &[(Locale::En, "Record")]),'
fresh = [(k, l) for k, l, _ in rows if ('("%s"' % k) not in t]
new = "\n".join('    ("%s", &[(Locale::En, "%s")]),' % (k, esc(l)) for k, l in fresh)
if new:
    t = t.replace(anchor, anchor + "\n" + new, 1)
open("crates/ui/src/strings.rs", "w", encoding="utf-8", newline="").write(t)
print("rows added:", len(fresh), "of", len(rows))
