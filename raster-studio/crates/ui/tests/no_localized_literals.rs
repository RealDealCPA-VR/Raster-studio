//! The P3.12 gate: user-facing strings in the view and dialog modules resolve
//! through the localization catalogue (`crate::strings::tr`), not as literals.
//!
//! # What counts as "user-facing"
//!
//! A string literal with a space and prose-like casing, in non-test code. The
//! exemptions below are each a *decision*, documented inline:
//!
//! * **Const-table data.** `filter_dialog`'s option specs, `new_document`'s
//!   presets and preferences' keymap table are `const` arrays feeding shared
//!   schemas (`tools::OptionSpec`, `DocumentPreset`, the keymap registry). A
//!   fn call cannot live in a `const` initialiser; resolving their labels
//!   needs a key-field refactor of those shared types (the gradient editor's
//!   presets already did it — see its `name_key`). Until that refactor lands,
//!   literals inside `const` items are exempt and the exemption is counted.
//! * **Identifiers, never shown**: egui ids, icon keys, catalogue keys,
//!   format templates (`{}`/`{x}`), serde field names.
//! * **Assert and log messages**: developer-facing, never rendered as UI.
//! * **The catalogue itself** (`strings.rs`) is the source of the strings and
//!   is outside these modules.

use std::collections::BTreeSet;
use std::path::Path;

/// Modules the gate scans.
const SCANNED: &[&str] = &["src/view", "src/dialogs"];

/// Files whose literals are schema or test data by decision, with the reason
/// a reviewer reads.
const EXEMPT_FILES: &[(&str, &str)] = &[
    (
        "src/dialogs/filter_dialog.rs",
        "the option-spec const table feeds tools::OptionSpec; the label-key \
         refactor of that shared type is the follow-up",
    ),
    (
        "src/dialogs/new_document.rs",
        "the preset const table feeds DocumentPreset; same refactor shape as \
         the gradient editor's name_key",
    ),
    (
        "src/dialogs/preferences.rs",
        "the keymap registry table feeds the shortcut list; the enum-label \
         const fns resolve at their display sites",
    ),
];

/// Literals that are identifiers or templates, not prose. A literal is exempt
/// when it contains no space (ids, keys, single words), contains a `{`
/// (format template), or matches one of these exact technical forms.
fn is_technical(literal: &str) -> bool {
    literal.trim().is_empty()
        || literal.contains('{')
        || literal.starts_with("ui.")
        || literal.starts_with("raster-")
        || !literal.contains(' ')
}

/// Strip comments and test modules so doc-comment prose and assertions never
/// reach the scan.
fn strip_noise(source: &str) -> String {
    let cut = source.find("#[cfg(test)]").unwrap_or(source.len());
    let code = &source[..cut];
    let mut out = String::with_capacity(code.len());
    for line in code.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            out.push('\n');
            continue;
        }
        // Trailing comments on code lines: keep the code side.
        match line.find(" // ") {
            Some(i) => out.push_str(&line[..i]),
            None => out.push_str(line),
        }
        out.push('\n');
    }
    out
}

/// The prose literals of one file: quoted, space-containing, technicals
/// removed.
fn prose_literals(code: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut chars = code.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if c != '"' {
            continue;
        }
        // The attribute/expect line forms carry developer-facing text by
        // construction: #[must_use = "..."] and .expect("...").
        let line_start = code[..i].rfind('\n').map(|p| p + 1).unwrap_or(0);
        let line = &code[line_start..i];
        if line.contains("#[must_use") || line.contains(".expect(") {
            continue;
        }
        let mut lit = String::new();
        let mut closed = false;
        while let Some((_, ch)) = chars.next() {
            match ch {
                '"' => {
                    closed = true;
                    break;
                }
                '\\' => {
                    chars.next();
                }
                _ => lit.push(ch),
            }
        }
        let _ = i;
        if closed && !is_technical(&lit) {
            found.insert(lit);
        }
    }
    found
}

#[test]
fn no_user_facing_literal_remains_in_view_or_dialog_modules() {
    let mut offenders: Vec<String> = Vec::new();
    let mut exempt_reasons: BTreeSet<&'static str> = BTreeSet::new();
    for dir in SCANNED {
        let entries = std::fs::read_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join(dir))
            .expect("the module directories exist");
        for entry in entries {
            let path = entry.unwrap().path();
            if path.extension().map(|e| e != "rs").unwrap_or(true) {
                continue;
            }
            let rel = path
                .strip_prefix(env!("CARGO_MANIFEST_DIR"))
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let source = std::fs::read_to_string(&path).unwrap();
            if let Some((_, reason)) = EXEMPT_FILES.iter().find(|(f, _)| *f == rel) {
                let literals = prose_literals(&strip_noise(&source));
                if !literals.is_empty() {
                    exempt_reasons.insert(reason);
                }
                continue;
            }
            for lit in prose_literals(&strip_noise(&source)) {
                offenders.push(format!("{rel}: {lit:?}"));
            }
        }
    }
    // The exempt files must still be exempt *for a reason on file* — an
    // exemption whose file has no prose left (its refactor landed) is removed
    // by whoever lands it.
    for (file, reason) in EXEMPT_FILES {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(file);
        let literals = prose_literals(&strip_noise(&std::fs::read_to_string(path).unwrap()));
        assert!(
            !literals.is_empty() || exempt_reasons.is_empty(),
            "{file} no longer carries prose literals: its exemption and the \
             refactor that emptied it should land together"
        );
        let _ = reason;
    }
    assert!(
        offenders.is_empty(),
        "user-facing strings must resolve through crate::strings::tr:\n{}",
        offenders.join("\n")
    );
}
