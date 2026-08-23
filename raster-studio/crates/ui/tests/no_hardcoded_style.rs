//! The design-system gate.
//!
//! The `ui` crate's claim is that a re-skin is an edit to one crate. That claim
//! is only true if nothing here names a colour, a font or a gap in numbers, and
//! that is the kind of rule that decays the moment somebody is in a hurry. So it
//! is checked mechanically, against this crate's own source, rather than trusted.
//!
//! Only the code that ships is scanned: everything from the first `#[cfg(test)]`
//! onward is cut, because a test that paints white on black to check geometry is
//! not a design decision.

use std::path::{Path, PathBuf};

/// Patterns that mean "a style value was written literally", and what to do
/// instead.
const FORBIDDEN: &[(&str, &str)] = &[
    ("Color32::WHITE", "use design::color32 with a ColorRole"),
    ("Color32::BLACK", "use design::color32 with a ColorRole"),
    ("Color32::RED", "use design::color32 with a ColorRole"),
    ("Color32::GREEN", "use design::color32 with a ColorRole"),
    ("Color32::BLUE", "use design::color32 with a ColorRole"),
    ("Color32::GRAY", "use design::color32 with a ColorRole"),
    ("Color32::from_rgb(", "use design::color32 with a ColorRole"),
    (
        "Color32::from_gray(",
        "use design::color32 with a ColorRole",
    ),
    (
        "FontId::new(",
        "use design::egui_theme::font_id with a TypeRole",
    ),
    ("FontId::proportional(", "use design::egui_theme::font_id"),
    ("FontId::monospace(", "use design::egui_theme::font_id"),
    (
        "Rounding::same(",
        "use design::egui_theme::rounding with a Radius",
    ),
    (
        "TextStyle::Heading",
        "use design::egui_theme::font_id with a TypeRole",
    ),
];

/// A literal number where a spacing token belongs.
///
/// Matched as "the call, then a digit", which catches `add_space(8.0)` while
/// leaving `add_space(Space::Small.pt())` alone.
const FORBIDDEN_NUMERIC: &[(&str, &str)] = &[
    ("add_space(", "use a design::Space rung"),
    (
        "Margin::same(",
        "use a design::Space rung or a Metrics field",
    ),
    ("Margin::symmetric(", "use a design::Space rung"),
    ("Stroke::new(", "use design's BorderWidths"),
];

/// The one sanctioned literal-colour call: converting the *user's* own
/// foreground or background, which is not the design system's to choose.
const SANCTIONED: &[&str] = &["Color32::from_rgba_unmultiplied("];

/// Directories owned by other work in this crate. They have their own gates.
const NOT_OURS: &[&str] = &["canvas", "dialogs"];

fn crate_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if NOT_OURS.contains(&name) {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The part of a file that ships: everything before its test module.
fn shipping_source(path: &Path) -> String {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    match text.find("#[cfg(test)]") {
        Some(at) => text[..at].to_string(),
        None => text,
    }
}

/// Strip comments, so a rule quoted in prose is not mistaken for a violation of
/// itself.
///
/// Cuts at the first `//`, which is sound for this crate because no shipping
/// line here contains `//` inside a string literal — and a gate that also had
/// to lex Rust strings would be a second thing to get wrong.
fn without_comments(line: &str) -> &str {
    match line.find("//") {
        Some(at) => &line[..at],
        None => line,
    }
}

#[test]
fn no_colour_font_or_radius_is_written_literally() {
    let mut files = Vec::new();
    rust_files(&crate_src(), &mut files);
    assert!(files.len() >= 10, "the crate lost its source files");

    let mut violations = Vec::new();
    for path in &files {
        for (number, line) in shipping_source(path).lines().enumerate() {
            let code = without_comments(line);
            if SANCTIONED.iter().any(|s| code.contains(s)) {
                continue;
            }
            for (pattern, fix) in FORBIDDEN {
                if code.contains(pattern) {
                    violations.push(format!(
                        "{}:{}: `{pattern}` — {fix}",
                        path.display(),
                        number + 1
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "style values written literally:\n{}",
        violations.join("\n")
    );
}

#[test]
fn no_spacing_or_stroke_width_is_a_bare_number() {
    let mut files = Vec::new();
    rust_files(&crate_src(), &mut files);

    let mut violations = Vec::new();
    for path in &files {
        for (number, line) in shipping_source(path).lines().enumerate() {
            let code = without_comments(line);
            for (call, fix) in FORBIDDEN_NUMERIC {
                let mut from = 0usize;
                while let Some(at) = code[from..].find(call) {
                    let after = from + at + call.len();
                    let next = code[after..].trim_start().chars().next();
                    if next.is_some_and(|c| c.is_ascii_digit()) {
                        violations.push(format!(
                            "{}:{}: `{call}` with a literal number — {fix}",
                            path.display(),
                            number + 1
                        ));
                    }
                    from = after;
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "spacing written as bare numbers:\n{}",
        violations.join("\n")
    );
}

#[test]
fn the_gate_actually_catches_something() {
    // A gate nobody has seen fail is a gate nobody knows works. Run the same
    // matcher over a line that *is* a violation and check it fires.
    let bad = "        painter.rect_filled(rect, Rounding::same(4.0), Color32::WHITE);";
    let hits: Vec<&str> = FORBIDDEN
        .iter()
        .filter(|(p, _)| bad.contains(p))
        .map(|(p, _)| *p)
        .collect();
    assert_eq!(hits.len(), 2, "expected both patterns to fire: {hits:?}");

    let numeric = "        ui.add_space(12.0);";
    let call = "add_space(";
    let after = numeric.find(call).unwrap() + call.len();
    assert!(numeric[after..]
        .trim_start()
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit()));

    // ...and that a token-driven line does not.
    let good = "        ui.add_space(Space::Small.pt());";
    let after = good.find(call).unwrap() + call.len();
    assert!(!good[after..]
        .trim_start()
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit()));
}

#[test]
fn a_doc_comment_quoting_a_rule_is_not_a_violation_of_it() {
    assert_eq!(
        without_comments("//! fails on a literal `Color32::from_rgb(`"),
        ""
    );
    assert_eq!(
        without_comments("    // Rounding::same(4.0) would be wrong here").trim(),
        ""
    );
    assert_eq!(
        without_comments("    let x = 1; // Color32::WHITE"),
        "    let x = 1; "
    );
}
