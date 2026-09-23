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

/// W2-X: a bare `[r, g, b, a]` of `f32` literals in shipping source is a
/// colour written in numbers — the Histogram's curves were `[1.0, 0.0, 0.0,
/// 1.0]` in `panels/histogram.rs`, which the call-site patterns above never
/// saw because no `Color32::` constructor was named. These are the lines
/// allowed to keep one, each *genuine user data* rather than a design
/// decision: the file's name, then text the line must contain.
const SANCTIONED_ARRAYS: &[(&str, &str, &str)] = &[
    (
        "color.rs",
        "DEFAULT_FOREGROUND",
        "the user's default foreground swatch (Photoshop's black)",
    ),
    (
        "color.rs",
        "DEFAULT_BACKGROUND",
        "the user's default background swatch (Photoshop's white)",
    ),
    (
        "menu.rs",
        "[1.0, 0.0, 0.0, 0.0]",
        "the Channel Mixer's identity row: mixer weights, not a colour",
    ),
    (
        "menu.rs",
        "[0.0, 1.0, 0.0, 0.0]",
        "the Channel Mixer's identity row: mixer weights, not a colour",
    ),
    (
        "menu.rs",
        "[0.0, 0.0, 1.0, 0.0]",
        "the Channel Mixer's identity row: mixer weights, not a colour",
    ),
    (
        "toolbar.rs",
        "return [0.0, 0.0, 0.0, 1.0];",
        "the preview of the user's gradient ramp when it has no stops: the ramp's own data",
    ),
];

/// Directories owned by other work in this crate. They have their own gates.
/// `panels` is *not* here: the panel models paint too (a histogram curve, a
/// channel tint) and are held to the same rule as `view`.
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

/// Is `rhs` a bare `f32` literal — nothing but digits, a sign, a dot and an
/// exponent? `52.0` fires; `Space::Small.pt()`, `x * 2.0` and `h - 52.0` do
/// not.
fn is_bare_f32(rhs: &str) -> bool {
    let trimmed = rhs.trim().trim_end_matches(';').trim();
    !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
        && trimmed.chars().any(|c| c.is_ascii_digit())
        && trimmed.contains('.')
}

/// Every bare four-float array literal on `code`: `[1.0, 0.0, 0.0, 1.0]`
/// fires; `[rgb[0], rgb[1], rgb[2], 1.0]`, `[0; 4]`, `[f32; 4]` and a
/// two-element pair do not.
fn bare_rgba_arrays(code: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut from = 0usize;
    while let Some(open) = code[from..].find('[') {
        let start = from + open;
        let Some(close) = code[start..].find(']') else {
            break;
        };
        let inner = &code[start + 1..start + close];
        let parts: Vec<&str> = inner.split(',').collect();
        if parts.len() == 4 && parts.iter().all(|p| is_bare_f32(p)) {
            found.push(&code[start..=start + close]);
        }
        from = start + 1;
    }
    found
}

/// W2-X: a colour written as four `f32` literals is a colour written
/// literally, whichever call it is handed to. The design crate has data
/// roles for the one class of colour that used to be argued as "data, not
/// design" — `ColorRole::ChannelRed` and its siblings — so the only bare
/// arrays left are the user's own swatch defaults, listed in
/// [`SANCTIONED_ARRAYS`] with their reason.
#[test]
fn no_colour_is_written_as_a_bare_rgba_array() {
    let mut files = Vec::new();
    rust_files(&crate_src(), &mut files);

    let mut violations = Vec::new();
    for path in &files {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        for (number, line) in shipping_source(path).lines().enumerate() {
            let code = without_comments(line);
            let arrays = bare_rgba_arrays(code);
            if arrays.is_empty() {
                continue;
            }
            let sanctioned = SANCTIONED_ARRAYS
                .iter()
                .any(|(file, text, _)| name == *file && code.contains(text));
            if sanctioned {
                continue;
            }
            violations.push(format!(
                "{}:{}: {} — name a design::ColorRole (data colours: ChannelRed / \
                 ChannelGreen / ChannelBlue / Luminance) or list genuine user data in \
                 SANCTIONED_ARRAYS",
                path.display(),
                number + 1,
                arrays.join(", ")
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "colours written as bare rgba arrays:\n{}",
        violations.join("\n")
    );
}

#[test]
fn the_panels_directory_is_in_the_scanned_set() {
    // The gate's reach is part of its claim: `src/panels` paints (the
    // Histogram's curves, the Channels tints) and must be scanned like
    // `src/view`, not skipped as another team's directory.
    let mut files = Vec::new();
    rust_files(&crate_src(), &mut files);
    let has = |suffix: &str| {
        files
            .iter()
            .any(|p| p.to_string_lossy().replace('\\', "/").ends_with(suffix))
    };
    assert!(
        has("src/panels/histogram.rs"),
        "panels/histogram.rs is not scanned"
    );
    assert!(has("src/panels/color.rs"), "panels/color.rs is not scanned");
    assert!(has("src/view/docks.rs"), "view/docks.rs is not scanned");
    assert!(!has("src/canvas/mod.rs"), "canvas has its own gate");
    // Every sanctioned array names a file that exists and a line that is
    // still there — a stale whitelist entry is a hole nobody is looking at.
    for (file, text, _) in SANCTIONED_ARRAYS {
        let path = files
            .iter()
            .find(|p| p.file_name().and_then(|n| n.to_str()) == Some(file))
            .unwrap_or_else(|| panic!("{file} is not in the scanned set"));
        assert!(
            shipping_source(path).lines().any(|l| l.contains(text)),
            "{file} no longer contains `{text}`: drop the stale whitelist entry"
        );
    }
}

/// C9: a bare `f32` `let` binding is a layout extent written in numbers.
///
/// The old `let footer_h = 52.0;` slipped past the call-site scan above —
/// the number sat in a `let`, not an argument. A gap, a height or an inset
/// must come from a `design` token, so any binding whose right-hand side is
/// a bare literal fails here, in the same scanned set as the other gates.
#[test]
fn no_bare_f32_let_binding_passes_as_a_layout_extent() {
    let mut files = Vec::new();
    rust_files(&crate_src(), &mut files);

    let mut violations = Vec::new();
    for path in &files {
        for (number, line) in shipping_source(path).lines().enumerate() {
            let code = without_comments(line);
            let Some(rest) = code.trim_start().strip_prefix("let ") else {
                continue;
            };
            let Some(eq) = rest.find('=') else {
                continue;
            };
            let name = rest[..eq].trim();
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            if is_bare_f32(&rest[eq + 1..]) {
                violations.push(format!(
                    "{}:{}: `let {name} = …;` is a bare f32 — use a design \
                     Space/Metrics/Radius token",
                    path.display(),
                    number + 1
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "layout extents written as bare f32 bindings:\n{}",
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

    // C9's regression: the footer height as a bare f32 `let` fires the
    // binding gate; a token-driven binding does not.
    assert!(is_bare_f32(" 52.0;"), "the old footer_h line must fire");
    assert!(is_bare_f32(" -4.0;"));
    assert!(!is_bare_f32(" Space::Small.pt();"));
    assert!(!is_bare_f32(" h - 52.0;"));
    assert!(!is_bare_f32(" x * 2.0;"));
    assert!(!is_bare_f32(" tokens.metrics.footer_height.pt();"));

    // W2-X's regression: the Histogram's literal primary fires the array
    // gate; a user-data conversion, an `[x; 4]` repeat and a type do not.
    assert_eq!(
        bare_rgba_arrays("        0 => [1.0, 0.0, 0.0, 1.0],"),
        vec!["[1.0, 0.0, 0.0, 1.0]"]
    );
    assert_eq!(
        bare_rgba_arrays("    pub const fn tint() -> [f32; 4] { [0.0, 0.0, 1.0, 0.6] }"),
        vec!["[0.0, 0.0, 1.0, 0.6]"]
    );
    assert!(bare_rgba_arrays("rgba_to_color32([rgb[0], rgb[1], rgb[2], 1.0])").is_empty());
    assert!(bare_rgba_arrays("let bins = [0; 4];").is_empty());
    assert!(bare_rgba_arrays("fn f(c: [f32; 4]) -> [f32; 4]").is_empty());
    assert!(bare_rgba_arrays("let pair = [1.0, 0.0];").is_empty());
    assert!(bare_rgba_arrays("let ints = [1, 2, 3, 4];").is_empty());
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
