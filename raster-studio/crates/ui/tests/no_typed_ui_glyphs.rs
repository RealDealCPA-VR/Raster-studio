//! The tofu gate.
//!
//! # What went wrong
//!
//! egui 0.29 loads three faces and no more: Ubuntu-Light, NotoEmoji and
//! emoji-icon-font. The `design` crate never replaces that stack, so the set of
//! characters this application can actually draw is fixed, small, and does not
//! contain most of the geometric symbols a designer reaches for. Every panel in
//! the app was typing those symbols into text widgets anyway — `"▸"` and `"✕"`
//! and `"⋯"` on the panel headers, fifteen of them in the Adjustments grid,
//! eleven in History, four in the Layers lock row, `"✓"` on every checked menu
//! item — and a character the font does not have is drawn as a tofu box. That
//! is what a user saw: empty squares, everywhere.
//!
//! `crates/ui/src/icons.rs` exists to prevent exactly this, and its own module
//! note says so. The tool palette was moved onto it; nothing else was.
//!
//! # The parts of the gate
//!
//! [`no_ui_source_string_holds_an_unrenderable_character`] lexes the
//! workspace's shipping source, pulls out every string literal, and fails on
//! any character outside [`ALLOWED`]. An icon has no business being a
//! character at all, so the allowlist holds only punctuation that belongs in a
//! *sentence*.
//!
//! It reads *every workspace member*, not just this crate — the member list
//! comes out of the workspace manifest, so `apps/studio-desktop` is in it too.
//! Scoping it to `crates/ui` is what let the Brush and Eraser option captions —
//! `tools::registry`'s `"Pressure → Size"`, painted into the options bar by
//! `ui::view::toolbar` — stay tofu through a whole round of this fix with the
//! suite green. It also decodes `\u{…}` escapes, because writing the character
//! that way is what hid four typed triangles in the Canvas Size dialog, two of
//! which egui has no glyph for.
//!
//! Reach is not membership, and the round after that failed on the difference.
//! [`shipping_source`] used to truncate each file at its first `#[cfg(test)]`,
//! and in `crates/app-shell/src/chrome.rs` that attribute sits on a helper
//! *function* four hundred lines above `mod tests`. The tab strip, the status
//! bar, the Preferences window and the empty state — `"No document open — File
//! ▸ Open, or drop an image here"`, the first sentence a new user reads, with a
//! U+25B8 in it — were all discarded before the scan began, and the runtime gate
//! could not cover the empty state either because it always opens a document.
//! [`shipping_source`] now cuts test items one at a time and keeps the rest of
//! the file, and [`the_scan_reaches_every_crate_and_drops_only_the_test_files`]
//! asserts on what the scan can *see* inside chrome.rs rather than on the file
//! being in a list.
//!
//! [`every_allowed_character_exists_in_the_font_egui_actually_loads`] then asks
//! egui's own font stack whether it has each allowlisted character. That is
//! what stops this gate degenerating: the cheap way to silence a source scan is
//! to widen its allowlist, and here a character that would be tofu cannot be
//! added to the allowlist without turning the suite red.
//!
//! [`every_icon_key_written_at_a_call_site_resolves_to_a_drawing`] closes the
//! door the first two leave open. An icon key is a plain `&str`, so a control
//! can now fail the *other* way — a typo, or a symbol pasted back in, resolves
//! to `Icon::UNKNOWN` and paints a hollow square that looks exactly like the
//! tofu box it replaced. This reads the key out of every call that takes one
//! and puts it through `ui::icons::ui_icon`.
//!
//! [`every_chrome_icon_key_is_claimed_by_a_control`] asks the same question
//! backwards: every key `ui::icons::CHROME_ICON_KEYS` declares has to be named
//! by some control. A drawing nobody asks for usually means a control that was
//! never converted and is still typing its symbol.
//!
//! One more gate lives outside this crate, in `app-shell`:
//! `nothing_the_chrome_paints_comes_out_as_a_tofu_box` draws the real chrome
//! headless, with every panel open and **every tool selected in turn**, and
//! checks every character it actually laid out — the claim about the
//! screenshot, rather than about the source. The tool loop is there because the
//! options bar only ever shows one tool's captions at a time, and one frame of
//! the default tool is what let the Brush's own captions through.

use std::path::{Path, PathBuf};

/// Non-ASCII characters that may appear in a string literal.
///
/// Every one is *text* — punctuation inside a sentence or between two readouts
/// — never a stand-in for an icon. An affordance takes a key from
/// `ui::icons::ui_icon` and is drawn.
const ALLOWED: &[(char, &str)] = &[
    ('\u{2014}', "em dash, in a sentence"),
    ('\u{00B7}', "middle dot, between two readouts"),
    ('\u{00D7}', "multiplication sign, between two dimensions"),
    ('\u{00B0}', "degree sign, on an angle"),
    ('\u{2026}', "ellipsis, on a menu item that opens a dialog"),
    (
        '\u{2022}',
        "bullet, the unsaved-changes mark inside a document title",
    ),
    ('\u{201C}', "left quote, around a file name in a sentence"),
    ('\u{201D}', "right quote, around a file name in a sentence"),
];

fn workspace_crates() -> PathBuf {
    workspace_root().join("crates")
}

fn workspace_root() -> PathBuf {
    // `crates/ui` -> `crates` -> the workspace.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the ui crate sits two directories below the workspace root")
        .to_path_buf()
}

fn crate_src() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every *workspace member's* `src`, not just this crate's and not just
/// `crates/`.
///
/// Scoping this gate to `crates/ui` is what let the bug survive its first fix:
/// the Brush and Eraser option captions live in `tools::registry`
/// (`"Pressure → Size"`, written with U+2192, which egui's fonts do not have),
/// `ui::view::toolbar` paints them straight into the options bar with
/// `ui.checkbox`, and the gate could not see them because it only ever read its
/// own crate. A label is a label wherever it is written down.
///
/// So the scan is the whole workspace rather than a list of crates somebody has
/// to remember to extend — and the member list is read out of the workspace
/// manifest rather than globbed from `crates/`, because `apps/studio-desktop`
/// and `tests/integration` are members too and a glob of `crates/*` silently
/// leaves them out. A crate with no UI in it costs nothing here: it has no
/// business holding a `"▸"` either, and at the time this was written the only
/// non-ASCII string literals outside `ui`/`tools`/`app-shell`/`design` in the
/// entire workspace were the PSD reader's own test fixtures — layer names in
/// Japanese, Greek and emoji — which [`all_label_files`] drops with the rest of
/// the test code.
fn source_roots() -> Vec<PathBuf> {
    let root = workspace_root();
    let mut roots = Vec::new();
    for member in workspace_members() {
        let src = root.join(&member).join("src");
        if src.is_dir() {
            roots.push(src);
        }
    }
    roots.sort();
    roots.dedup();
    assert!(
        roots.len() >= 20,
        "found only {} members to scan; the workspace layout moved under this gate",
        roots.len()
    );
    assert!(
        roots.contains(&crate_src()),
        "the ui crate itself dropped out of the scan"
    );
    assert!(
        roots.contains(
            &workspace_root()
                .join("apps")
                .join("studio-desktop")
                .join("src")
        ),
        "the desktop binary dropped out of the scan"
    );
    roots
}

/// The `members = [ … ]` of the workspace manifest, as relative directories.
///
/// A three-line reader rather than a TOML dependency: the array is the only
/// thing wanted, and the assertions in [`source_roots`] fail loudly if the shape
/// of the manifest ever changes under it.
fn workspace_members() -> Vec<String> {
    let manifest = workspace_root().join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .unwrap_or_else(|e| panic!("read {}: {e}", manifest.display()));
    let start = text
        .find("members")
        .and_then(|at| text[at..].find('[').map(|b| at + b + 1))
        .expect("the workspace manifest declares `members = [ … ]`");
    let end = start
        + text[start..]
            .find(']')
            .expect("the members array is closed");
    text[start..end]
        .split(',')
        .filter_map(|entry| {
            let entry = entry.trim();
            entry
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .map(str::to_string)
        })
        .collect()
}

/// Every shipping `.rs` file under every source root.
///
/// "Shipping" excludes the test bodies that live in a file of their own —
/// `#[cfg(test)] mod tests;` next to `tests.rs`, and the `#[path]` form
/// `crates/app-shell` uses for `editor_tests.rs`. [`shipping_source`] cuts the
/// *in-file* `#[cfg(test)]` items — a `mod tests { … }`, and also a lone helper
/// function or `use` — for the same reason: a test asserting on a PSD layer
/// named "日本語" is not drawing anything. The excluded set is read out of the
/// source rather than guessed from the file name, so a shipping module that
/// happens to be called `tests.rs` would still be scanned.
fn all_label_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for root in source_roots() {
        rust_files(&root, &mut files);
    }
    let excluded: Vec<PathBuf> = files
        .iter()
        .flat_map(|path| {
            let text = std::fs::read_to_string(path).unwrap_or_default();
            let dir = path.parent().expect("a file has a directory").to_path_buf();
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            // `foo.rs` owns `foo/bar.rs`; `mod.rs` owns its own directory.
            let owned = if stem == "mod" || stem == "lib" || stem == "main" {
                dir.clone()
            } else {
                dir.join(&stem)
            };
            test_only_modules(&text).into_iter().flat_map(move |name| {
                if name.ends_with(".rs") {
                    // A `#[path = "…"]`, relative to the declaring file.
                    vec![dir.join(&name)]
                } else {
                    vec![
                        owned.join(format!("{name}.rs")),
                        owned.join(&name).join("mod.rs"),
                    ]
                }
            })
        })
        .collect();
    files.retain(|path| !excluded.contains(path));
    files
}

/// The out-of-line modules a `#[cfg(test)]` declares, as either a `#[path]`
/// file name (ending in `.rs`) or a bare module name.
///
/// Deliberately narrow: it reads only the attributes and the `mod NAME;` that
/// immediately follow the `#[cfg(test)]`, and an inline `mod NAME { … }` is
/// ignored because [`shipping_source`] already cuts those.
fn test_only_modules(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = src;
    while let Some(at) = rest.find("#[cfg(test)]") {
        rest = &rest[at + "#[cfg(test)]".len()..];
        // Everything up to the `mod` the attributes belong to, and never more
        // than a couple of lines of it.
        let mut end = rest.find("mod ").unwrap_or(rest.len()).min(200);
        while end > 0 && !rest.is_char_boundary(end) {
            end -= 1;
        }
        let window = &rest[..end];
        if let Some(p) = window.find("#[path = \"") {
            let after = &window[p + "#[path = \"".len()..];
            if let Some(q) = after.find('"') {
                out.push(after[..q].to_string());
                continue;
            }
        }
        // Only whitespace between the attribute and the `mod`, or this
        // `#[cfg(test)]` belongs to something else entirely.
        if !window.trim().is_empty() || end == rest.len() {
            continue;
        }
        let after = &rest[end + "mod ".len()..];
        let name: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let terminator = after[name.len()..].trim_start().chars().next();
        if !name.is_empty() && terminator == Some(';') {
            out.push(name);
        }
    }
    out
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// The part of a file that ships: everything except its `#[cfg(test)]` items.
///
/// A test that asserts on a PSD layer named "日本語" is not drawing anything, so
/// test code comes out. But it has to come out *item by item*.
///
/// This used to truncate the file at the first `#[cfg(test)]`, and that is the
/// hole this round was rejected for. `crates/app-shell/src/chrome.rs` carries a
/// `#[cfg(test)] fn harvest_workspace_for_test` at line 493 and does not open
/// `mod tests` until line 886, so ~390 lines holding the tab strip, the
/// empty-state message, the status bar and the Preferences window were thrown
/// away before any of these tests looked at them — including
/// `"No document open — File ▸ Open, or drop an image here"`, a tofu box in the
/// first sentence a new user reads, and the tab strip's own
/// `ui_icon_button_id(ui, "close", …)`. `crates/compositor/src/testkit.rs`
/// mentions the attribute in a *doc comment* on line 1 and lost its whole file
/// the same way.
///
/// So: cut each `#[cfg(test)]` item and keep everything after it. Newlines
/// inside a cut are kept so the line numbers this gate reports still match the
/// file. The attribute is found with the same lexer the rest of this file uses,
/// which is what stops a mention of it inside a comment or a string from cutting
/// anything.
fn shipping_source(path: &Path) -> String {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    strip_test_items(&text)
}

/// Every `#[cfg(test)]` item removed from `src`, newlines preserved.
fn strip_test_items(src: &str) -> String {
    const ATTR: &str = "#[cfg(test)]";
    let attr: Vec<char> = ATTR.chars().collect();
    let c: Vec<char> = src.chars().collect();
    let mut out = String::new();
    let mut i = 0usize;
    while i < c.len() {
        // A comment or a literal is copied through whole, so `#[cfg(test)]`
        // written inside one cuts nothing.
        if let Some(next) = skip_opaque(&c, i) {
            out.extend(c[i..next].iter());
            i = next;
            continue;
        }
        if c[i] == '#' && i + attr.len() <= c.len() && c[i..i + attr.len()] == attr[..] {
            let end = end_of_item(&c, i + attr.len());
            // Keep the line count, drop the text.
            out.extend(c[i..end].iter().filter(|ch| **ch == '\n'));
            i = end;
            continue;
        }
        out.push(c[i]);
        i += 1;
    }
    out
}

/// One comment, string literal or char literal starting at `at`, as the index
/// just past it — or `None` if `at` does not start one.
///
/// The single place this file decides what is code and what is not, shared by
/// the stripper and the brace matcher so they cannot disagree.
fn skip_opaque(c: &[char], at: usize) -> Option<usize> {
    let ch = *c.get(at)?;
    if ch == '/' && c.get(at + 1) == Some(&'/') {
        let mut i = at + 2;
        while i < c.len() && c[i] != '\n' {
            i += 1;
        }
        return Some(i);
    }
    if ch == '/' && c.get(at + 1) == Some(&'*') {
        let mut depth = 1usize;
        let mut i = at + 2;
        while i < c.len() && depth > 0 {
            if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                depth += 1;
                i += 2;
            } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                depth -= 1;
                i += 2;
            } else {
                i += 1;
            }
        }
        return Some(i);
    }
    // A raw string, `r"…"` / `r#"…"#`, possibly byte-prefixed. Only when the
    // `r` starts a token, so the `r` at the end of an identifier is left alone.
    if ch == 'r' && !c.get(at.wrapping_sub(1)).is_some_and(is_ident) {
        let mut j = at + 1;
        let mut hashes = 0usize;
        while c.get(j) == Some(&'#') {
            hashes += 1;
            j += 1;
        }
        if c.get(j) == Some(&'"') {
            j += 1;
            while j < c.len() {
                if c[j] == '"' {
                    let mut k = j + 1;
                    let mut seen = 0usize;
                    while seen < hashes && c.get(k) == Some(&'#') {
                        seen += 1;
                        k += 1;
                    }
                    if seen == hashes {
                        return Some(k);
                    }
                }
                j += 1;
            }
            return Some(c.len());
        }
    }
    if ch == '"' {
        let mut j = at + 1;
        while j < c.len() {
            if c[j] == '\\' {
                j += 2;
                continue;
            }
            if c[j] == '"' {
                return Some(j + 1);
            }
            j += 1;
        }
        return Some(c.len());
    }
    if ch == '\'' {
        // `'\n'` and friends.
        if c.get(at + 1) == Some(&'\\') {
            let mut j = at + 2;
            while j < c.len() && c[j] != '\'' {
                j += 1;
            }
            return Some((j + 1).min(c.len()));
        }
        // `'x'` — but `'a` is a lifetime and is code.
        if c.get(at + 2) == Some(&'\'') {
            return Some(at + 3);
        }
    }
    None
}

fn is_ident(c: &char) -> bool {
    c.is_alphanumeric() || *c == '_'
}

/// The index just past the item that starts at or after `at`.
///
/// `at` is the first character after a `#[cfg(test)]`. Further attributes, doc
/// comments and whitespace are stepped over, then the item runs to whichever
/// comes first at the top level: a `{ … }` block, which ends it — `mod tests
/// { … }`, `fn helper() { … }`, `impl … { … }` — or a `;`, which ends the
/// declaration forms — `mod tests;`, `use super::*;`, `static X: T = y;`.
/// Brackets and parentheses are counted so the `;` inside `[u8; 3]` and the
/// `{ … }` inside a `Lazy::new(|| { … })` are not mistaken for the end.
fn end_of_item(c: &[char], at: usize) -> usize {
    let mut i = at;
    // The run-up: whitespace, comments, and any further attributes.
    loop {
        while i < c.len() && c[i].is_whitespace() {
            i += 1;
        }
        if i < c.len() && c[i] == '/' && matches!(c.get(i + 1), Some('/') | Some('*')) {
            i = skip_opaque(c, i).unwrap_or(c.len());
            continue;
        }
        if i < c.len() && c[i] == '#' {
            let mut j = i + 1;
            if c.get(j) == Some(&'!') {
                j += 1;
            }
            if c.get(j) == Some(&'[') {
                i = match_delimiter(c, j);
                continue;
            }
        }
        break;
    }
    // The item itself.
    let mut depth = 0usize;
    while i < c.len() {
        if let Some(next) = skip_opaque(c, i) {
            i = next;
            continue;
        }
        match c[i] {
            '(' | '[' => depth += 1,
            ')' | ']' => depth = depth.saturating_sub(1),
            '{' => {
                if depth == 0 {
                    return match_delimiter(c, i);
                }
                depth += 1;
            }
            '}' => depth = depth.saturating_sub(1),
            ';' if depth == 0 => return i + 1,
            _ => {}
        }
        i += 1;
    }
    c.len()
}

/// The index just past the delimiter matching the opener at `at`.
///
/// Blind inside comments and literals, so a `}` in a string does not close a
/// block.
fn match_delimiter(c: &[char], at: usize) -> usize {
    let mut depth = 0usize;
    let mut i = at;
    while i < c.len() {
        if let Some(next) = skip_opaque(c, i) {
            i = next;
            continue;
        }
        match c[i] {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                if depth <= 1 {
                    return i + 1;
                }
                depth -= 1;
            }
            _ => {}
        }
        i += 1;
    }
    c.len()
}

/// A `\u{…}` escape starting at `at`, as the character it means and the index
/// just past the closing brace.
fn unicode_escape(c: &[char], at: usize) -> Option<(char, usize)> {
    if c.get(at + 1) != Some(&'u') || c.get(at + 2) != Some(&'{') {
        return None;
    }
    let mut hex = String::new();
    let mut j = at + 3;
    while j < c.len() && c[j] != '}' {
        hex.push(c[j]);
        j += 1;
    }
    if j >= c.len() {
        return None;
    }
    let code = u32::from_str_radix(&hex, 16).ok()?;
    Some((char::from_u32(code)?, j + 1))
}

/// Every string literal in `src`, with the line it starts on.
///
/// A real little lexer rather than a regex, because the three things that would
/// otherwise produce false positives all need one: a `//` inside a string, a
/// quote inside a comment, and a raw string carrying either. It skips char
/// literals too — `tool_for_key('§')` is a key press, not a label.
fn string_literals(src: &str) -> Vec<(usize, String)> {
    let c: Vec<char> = src.chars().collect();
    let mut out = Vec::new();
    let mut line = 1usize;
    let mut i = 0usize;
    while i < c.len() {
        // A line comment, doc comment included: to the end of the line.
        if c[i] == '/' && i + 1 < c.len() && c[i + 1] == '/' {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
            continue;
        }
        // A block comment, which Rust allows to nest.
        if c[i] == '/' && i + 1 < c.len() && c[i + 1] == '*' {
            let mut depth = 1usize;
            i += 2;
            while i < c.len() && depth > 0 {
                if c[i] == '\n' {
                    line += 1;
                    i += 1;
                } else if c[i] == '/' && i + 1 < c.len() && c[i + 1] == '*' {
                    depth += 1;
                    i += 2;
                } else if c[i] == '*' && i + 1 < c.len() && c[i + 1] == '/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            continue;
        }
        // A raw string: r"…", r#"…"#, and the byte forms, which take no escapes
        // and end only on a quote followed by the same run of hashes.
        if c[i] == 'r' {
            let mut j = i + 1;
            let mut hashes = 0usize;
            while j < c.len() && c[j] == '#' {
                hashes += 1;
                j += 1;
            }
            if j < c.len() && c[j] == '"' {
                let start = line;
                let mut content = String::new();
                j += 1;
                while j < c.len() {
                    if c[j] == '"' {
                        let mut k = j + 1;
                        let mut seen = 0usize;
                        while k < c.len() && seen < hashes && c[k] == '#' {
                            seen += 1;
                            k += 1;
                        }
                        if seen == hashes {
                            j = k;
                            break;
                        }
                    }
                    if c[j] == '\n' {
                        line += 1;
                    }
                    content.push(c[j]);
                    j += 1;
                }
                out.push((start, content));
                i = j;
                continue;
            }
        }
        if c[i] == '"' {
            let start = line;
            let mut content = String::new();
            let mut j = i + 1;
            while j < c.len() {
                if c[j] == '\\' {
                    // `"\u{25B2}"` *is* a triangle: it compiles to the same
                    // string as typing the character, and writing it this way
                    // is how four of them sat in the Canvas Size dialog while
                    // the scan read straight past. Decode it and judge the
                    // character it produces.
                    if let Some((ch, next)) = unicode_escape(&c, j) {
                        content.push(ch);
                        j = next;
                        continue;
                    }
                    j += 2;
                    continue;
                }
                if c[j] == '"' {
                    j += 1;
                    break;
                }
                if c[j] == '\n' {
                    line += 1;
                }
                content.push(c[j]);
                j += 1;
            }
            out.push((start, content));
            i = j;
            continue;
        }
        // A char literal — or a lifetime, which looks the same for one char.
        if c[i] == '\'' {
            if i + 1 < c.len() && c[i + 1] == '\\' {
                let mut j = i + 2;
                while j < c.len() && c[j] != '\'' {
                    j += 1;
                }
                i = j + 1;
                continue;
            }
            if i + 2 < c.len() && c[i + 2] == '\'' {
                i += 3;
                continue;
            }
            i += 1;
            continue;
        }
        if c[i] == '\n' {
            line += 1;
        }
        i += 1;
    }
    out
}

/// Every character of `src`'s string literals that is not ASCII and not
/// allowlisted, as `(line, character)`.
fn offending(src: &str) -> Vec<(usize, char)> {
    let mut out = Vec::new();
    for (line, literal) in string_literals(src) {
        for ch in literal.chars() {
            if !ch.is_ascii() && !ALLOWED.iter().any(|(a, _)| *a == ch) {
                out.push((line, ch));
            }
        }
    }
    out
}

/// The functions that take an icon *key*, and which argument the key is.
///
/// A key is a plain `&str`, so a typo — or a symbol pasted back in out of
/// habit — compiles. It would then paint `Icon::UNKNOWN` and nothing else would
/// notice, which is the same blank-control bug in a new hat.
const KEY_TAKERS: &[(&str, usize)] = &[
    ("icon_toggle", 1),
    ("icon_toggle_id", 1),
    ("icon_button_id", 1),
    ("ui_icon_button", 1),
    ("ui_icon_button_id", 1),
    ("paint_icon", 2),
    ("paint_ui_icon", 2),
    ("ui_icon", 0),
];

/// The top-level arguments of every call to `callee` in `src`, as source text.
///
/// Balanced over brackets and blind inside string literals, so a call spread
/// across five lines — which `rustfmt` produces for most of these — is read as
/// one call, and a comma inside a nested call or a `[a, b]` is not a separator.
fn call_arguments(src: &str, callee: &str) -> Vec<Vec<String>> {
    let c: Vec<char> = src.chars().collect();
    let name: Vec<char> = callee.chars().collect();
    let mut calls = Vec::new();
    let mut i = 0usize;
    while i + name.len() < c.len() {
        let matched = c[i..i + name.len()] == name[..]
            && c[i + name.len()] == '('
            && (i == 0 || !(c[i - 1].is_alphanumeric() || c[i - 1] == '_'));
        if !matched {
            i += 1;
            continue;
        }
        let mut depth = 0usize;
        let mut args = Vec::new();
        let mut current = String::new();
        let mut j = i + name.len();
        let mut in_string = false;
        while j < c.len() {
            let ch = c[j];
            if in_string {
                current.push(ch);
                if ch == '\\' {
                    if j + 1 < c.len() {
                        current.push(c[j + 1]);
                    }
                    j += 2;
                    continue;
                }
                if ch == '"' {
                    in_string = false;
                }
                j += 1;
                continue;
            }
            match ch {
                '"' => {
                    in_string = true;
                    current.push(ch);
                }
                '(' | '[' | '{' => {
                    depth += 1;
                    if depth > 1 {
                        current.push(ch);
                    }
                }
                ')' | ']' | '}' => {
                    depth -= 1;
                    if depth == 0 {
                        args.push(current.trim().to_string());
                        break;
                    }
                    current.push(ch);
                }
                ',' if depth == 1 => {
                    args.push(current.trim().to_string());
                    current = String::new();
                }
                _ => current.push(ch),
            }
            j += 1;
        }
        // `rustfmt` puts a trailing comma on every multi-line call, which leaves
        // an empty last argument. Dropping it keeps the indices the same as the
        // ones a reader counts in the signature.
        if args.last().is_some_and(String::is_empty) {
            args.pop();
        }
        calls.push(args);
        i = j.max(i + 1);
    }
    calls
}

#[test]
fn every_icon_key_written_at_a_call_site_resolves_to_a_drawing() {
    // A key that resolves to nothing paints `Icon::UNKNOWN` — a hollow square,
    // which is exactly what the tofu box looked like. The registry gates in
    // `icons.rs` prove every key the *enums* name has a drawing; this proves it
    // for every key a call site spells out by hand.
    let files = all_label_files();

    let mut checked = 0usize;
    let mut bad = Vec::new();
    for path in &files {
        let src = shipping_source(path);
        for (callee, index) in KEY_TAKERS {
            for args in call_arguments(&src, callee) {
                let Some(arg) = args.get(*index) else {
                    continue;
                };
                // Only literals: a key held in a variable is checked where it
                // was produced, and every producer is gated through its enum.
                let Some(key) = arg
                    .strip_prefix('"')
                    .and_then(|rest| rest.strip_suffix('"'))
                else {
                    continue;
                };
                checked += 1;
                if ui::icons::ui_icon(key).is_unknown() {
                    bad.push(format!(
                        "{}: {callee}(.., {arg}, ..) — no drawing for that key",
                        path.display()
                    ));
                }
            }
        }
    }
    assert!(
        checked >= 10,
        "found only {checked} literal icon keys; the scanner is not matching \
         the call sites any more"
    );
    assert!(
        bad.is_empty(),
        "an icon key with no drawing would paint a hollow square:\n{}",
        bad.join("\n")
    );
}

#[test]
fn every_chrome_icon_key_is_claimed_by_a_control() {
    // The other direction. `icons.rs` proves every key in `CHROME_ICON_KEYS`
    // has a drawing; this proves every drawing has a caller. A key with no
    // call site is either a control that was never converted — the state this
    // whole gate exists to end — or dead weight the next reader has to decide
    // about. Either way somebody should know.
    //
    // `icons.rs` itself is skipped: it holds the list and the drawing table, so
    // every key appears there twice by construction and counting it would make
    // the test pass on nothing.
    let declares = crate_src().join("icons.rs");
    let files = all_label_files();

    let mut orphans = Vec::new();
    for key in ui::icons::CHROME_ICON_KEYS {
        let callers = files
            .iter()
            .filter(|path| **path != declares)
            .filter(|path| {
                string_literals(&shipping_source(path))
                    .iter()
                    .any(|(_, literal)| literal == key)
            })
            .count();
        if callers == 0 {
            orphans.push(*key);
        }
    }
    assert!(
        orphans.is_empty(),
        "these chrome icons are drawn but nothing asks for them: {orphans:?}. \
         Either a control is still typing a symbol instead of naming the key, \
         or the drawing should go."
    );
}

#[test]
fn the_scan_reaches_every_crate_and_drops_only_the_test_files() {
    // The gate is only as good as its reach, and the last round's miss was a
    // reach problem rather than a logic one. So: name the files that must be in
    // and the files that must be out, rather than trusting a count.
    let files = all_label_files();
    let crates = workspace_crates();
    let chrome = crates.join("app-shell").join("src").join("chrome.rs");
    for wanted in [
        // The file the first round missed.
        crates.join("tools").join("src").join("registry.rs"),
        chrome.clone(),
        crates.join("design").join("src").join("widgets.rs"),
        crates.join("ui").join("src").join("icons.rs"),
        // A crate with no UI in it is scanned too — a shipping file, not a
        // fixture.
        crates.join("psd").join("src").join("model.rs"),
        // Not under `crates/` at all. A workspace member all the same, and a
        // label written here would be on screen like any other.
        workspace_root()
            .join("apps")
            .join("studio-desktop")
            .join("src")
            .join("main.rs"),
    ] {
        assert!(
            files.contains(&wanted),
            "{} is not being scanned",
            wanted.display()
        );
    }

    // Membership is not reach. The second round *listed* chrome.rs and then
    // discarded 76% of it, because `shipping_source` truncated the file at the
    // `#[cfg(test)]` on a helper function at line 493 and `mod tests` does not
    // start until line 886. So assert on what the scan can actually see: the
    // empty-state message, which lives at line 562 in between, must be in the
    // text, and a name from inside `mod tests` must not.
    let scanned = shipping_source(&chrome);
    assert!(
        scanned.contains("raster-start-screen"),
        "the start screen is inside chrome.rs but not inside what the \
         scan reads — `shipping_source` is truncating the file again"
    );
    assert!(
        scanned.contains("ui_icon_button_id"),
        "the tab strip's close button is inside chrome.rs but not inside what \
         the scan reads"
    );
    assert!(
        !scanned.contains("nothing_the_chrome_paints_comes_out_as_a_tofu_box"),
        "chrome.rs's `mod tests` is being scanned as shipping source"
    );
    // ...and the line numbering survived the cut, so a violation this gate
    // reports can still be found by the line it names.
    assert_eq!(
        scanned.lines().count(),
        std::fs::read_to_string(&chrome)
            .expect("read chrome.rs")
            .lines()
            .count(),
        "cutting the test items moved the line numbers"
    );
    for unwanted in [
        // `#[cfg(test)] #[path = "editor_tests.rs"] mod tests;`
        crates.join("app-shell").join("src").join("editor_tests.rs"),
        // `#[cfg(test)] mod tests;` — PSD layer names in Japanese and Greek.
        crates.join("psd").join("src").join("tests.rs"),
    ] {
        assert!(
            !files.contains(&unwanted),
            "{} is test code and should not be scanned",
            unwanted.display()
        );
        assert!(unwanted.is_file(), "{} moved", unwanted.display());
    }
}

#[test]
fn the_stripper_removes_a_test_item_and_keeps_what_follows_it() {
    // The rejected round's bug, in miniature: a `#[cfg(test)]` helper *function*
    // sitting above hundreds of lines of shipping code. Everything after it has
    // to survive.
    let src = concat!(
        "fn ship_one() {}\n",
        "#[cfg(test)]\n",
        "fn helper(&self, a: [u8; 3]) -> Out {\n",
        "    let s = \"}\";\n",
        "    Out\n",
        "}\n",
        "fn ship_two() { label(\"kept\"); }\n",
    );
    let out = strip_test_items(src);
    assert!(out.contains("ship_one"));
    assert!(
        out.contains("ship_two"),
        "the rest of the file was truncated"
    );
    assert!(out.contains("kept"));
    assert!(!out.contains("helper"), "the test item survived: {out:?}");
    // A `}` inside a string does not close the body early.
    assert!(!out.contains("Out"), "{out:?}");
    // Line numbering is untouched, which is what the reported `path:line` means.
    assert_eq!(out.lines().count(), src.lines().count());

    // The inline module form, and code after it.
    let with_mod = concat!(
        "fn ship() {}\n",
        "#[cfg(test)]\n",
        "mod tests {\n",
        "    fn t() { let j = \"日本語\"; }\n",
        "}\n",
        "fn after() { label(\"also kept\"); }\n",
    );
    let out = strip_test_items(with_mod);
    assert!(out.contains("also kept"));
    assert!(!out.contains("日本語"));

    // The declaration forms end at their semicolon, not at a later brace.
    let decls = "#[cfg(test)]\nmod tests;\nfn after() {}\n";
    let out = strip_test_items(decls);
    assert!(!out.contains("mod tests"));
    assert!(out.contains("after"));
    let uses = "#[cfg(test)]\nuse super::*;\nfn after() {}\n";
    assert!(strip_test_items(uses).contains("after"));
    assert!(!strip_test_items(uses).contains("super"));

    // A statement whose initialiser holds a brace inside parentheses.
    let stmt = "#[cfg(test)]\nthread_local!(static X: C = C::new(|| { 1 }););\nfn after() {}\n";
    let out = strip_test_items(stmt);
    assert!(out.contains("after"), "{out:?}");
    assert!(!out.contains("thread_local"), "{out:?}");

    // A `thread_local! { … }` in its brace form ends at the brace.
    let braces = "#[cfg(test)]\nthread_local! { static X: u8 = 1; }\nfn after() {}\n";
    let out = strip_test_items(braces);
    assert!(out.contains("after"), "{out:?}");
    assert!(!out.contains("thread_local"), "{out:?}");

    // Further attributes between the `#[cfg(test)]` and the item are part of it.
    let attrs = "#[cfg(test)]\n#[allow(dead_code)]\nfn helper() {}\nfn after() {}\n";
    let out = strip_test_items(attrs);
    assert!(out.contains("after"));
    assert!(!out.contains("dead_code"));
}

#[test]
fn the_stripper_ignores_the_attribute_written_inside_prose_or_a_string() {
    // `crates/compositor/src/testkit.rs` names `#[cfg(test)]` in its first doc
    // comment, and the truncating version threw the whole file away over it.
    let doc = "//! built behind `#[cfg(test)]`.\nfn ship() { label(\"kept\"); }\n";
    assert!(strip_test_items(doc).contains("kept"));
    let block = "/* #[cfg(test)] */\nfn ship() { label(\"kept\"); }\n";
    assert!(strip_test_items(block).contains("kept"));
    let string = "let s = \"#[cfg(test)]\";\nfn ship() { label(\"kept\"); }\n";
    assert!(strip_test_items(string).contains("kept"));
    let raw = "let s = r#\"#[cfg(test)]\"#;\nfn ship() { label(\"kept\"); }\n";
    assert!(strip_test_items(raw).contains("kept"));
    // A lifetime is not a char literal, so it does not swallow the code after it.
    let lifetime = "fn f<'a>(x: &'a str) { label(\"kept\"); }\n";
    assert!(strip_test_items(lifetime).contains("kept"));
}

#[test]
fn the_test_module_reader_tells_a_file_module_from_an_inline_one() {
    assert_eq!(
        test_only_modules("#[cfg(test)]\nmod tests;\n"),
        vec!["tests".to_string()]
    );
    assert_eq!(
        test_only_modules("#[cfg(test)]\n#[path = \"editor_tests.rs\"]\nmod tests;\n"),
        vec!["editor_tests.rs".to_string()]
    );
    // An inline module is cut by `shipping_source`, not by this.
    assert!(test_only_modules("#[cfg(test)]\nmod tests {\n").is_empty());
    // A `#[cfg(test)]` on a function names no module.
    assert!(test_only_modules("#[cfg(test)]\nfn helper() {}\nmod real;\n").is_empty());
    // A shipping module declared further down is not swept up with it.
    assert!(test_only_modules("#[cfg(test)]\nuse x;\nmod real;\n").is_empty());
}

#[test]
fn the_call_site_scanner_reads_a_multi_line_call_correctly() {
    let src = "    icon_toggle_id(\n        ui,\n        \"overflow\",\n        open,\n        \"Move this panel\",\n        Some(ids::panel_menu(panel, [1, 2])),\n    )\n";
    let calls = call_arguments(src, "icon_toggle_id");
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0][0], "ui");
    assert_eq!(calls[0][1], "\"overflow\"");
    assert_eq!(calls[0][3], "\"Move this panel\"");
    // The nested call's own commas did not split the argument list.
    assert_eq!(calls[0].len(), 5, "{:?}", calls[0]);

    // A comma inside a string is not a separator either.
    let tricky = "icon_toggle(ui, \"close\", false, \"Close, really\")";
    let calls = call_arguments(tricky, "icon_toggle");
    assert_eq!(calls[0].len(), 4, "{:?}", calls[0]);
    assert_eq!(calls[0][3], "\"Close, really\"");

    // `icon_toggle` must not also match inside `icon_toggle_id`.
    assert!(call_arguments("icon_toggle_id(ui, \"close\", a, b, c)", "icon_toggle").is_empty());
}

#[test]
fn no_ui_source_string_holds_an_unrenderable_character() {
    let files = all_label_files();
    assert!(files.len() >= 10, "the crate lost its source files");

    let mut violations = Vec::new();
    for path in &files {
        for (line, ch) in offending(&shipping_source(path)) {
            violations.push(format!(
                "{}:{line}: U+{:04X} {ch:?} in a string literal",
                path.display(),
                ch as u32
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "a symbol is being typed where a drawing belongs. egui's font stack has \
         no glyph for most of these, so each one is a tofu box on screen. Add a \
         drawing to `ui::icons` and pass its key instead:\n{}",
        violations.join("\n")
    );
}

#[test]
fn every_allowed_character_exists_in_the_font_egui_actually_loads() {
    // The load-bearing half. An allowlist is only honest if a character cannot
    // be added to it to hide a tofu box, so each entry is put to the very fonts
    // the application draws with.
    let ctx = egui::Context::default();
    design::apply_theme(&ctx, design::Theme::Dark);
    let _ = ctx.run(Default::default(), |_| {});
    let font = egui::FontId::new(13.0, egui::FontFamily::Proportional);

    let mut missing = Vec::new();
    ctx.fonts(|f| {
        for (ch, why) in ALLOWED {
            if !f.has_glyph(&font, *ch) {
                missing.push(format!("U+{:04X} {ch:?} ({why})", *ch as u32));
            }
        }
    });
    assert!(
        missing.is_empty(),
        "allowlisted as text, but egui has no glyph for it — it would draw as a \
         tofu box:\n{}",
        missing.join("\n")
    );
}

#[test]
fn the_gate_catches_the_symbols_this_crate_used_to_type() {
    // A gate nobody has seen fail is a gate nobody knows works. This is the
    // real shape of the code that shipped, one line from each surface the bug
    // was found on, run through the same scanner.
    let was = concat!(
        "fn header(ui: &mut Ui, collapsed: bool) {\n",
        "    glyph_toggle(ui, if collapsed { \"\u{25B8}\" } else { \"\u{25BE}\" }, true, \"\");\n",
        "    glyph_toggle(ui, \"\u{2715}\", false, \"Close panel\");\n",
        "    glyph_toggle_id(ui, \"\u{22EF}\", open, \"Move this panel\", id);\n",
        "    let brightness = \"\u{25D0}\";\n",
        "    let painted = \"\u{270E}\";\n",
        "    let lock = \"\u{25A8}\";\n",
        "}\n",
    );
    let hits = offending(was);
    assert_eq!(
        hits.len(),
        7,
        "expected all seven symbols to be caught, got {hits:?}"
    );
    assert_eq!(hits[0], (2, '\u{25B8}'));

    // ...and the same file with keys instead of symbols is clean.
    let now = concat!(
        "fn header(ui: &mut Ui, collapsed: bool) {\n",
        "    let chevron = if collapsed { \"chevron-right\" } else { \"chevron-down\" };\n",
        "    icon_toggle(ui, chevron, true, \"\");\n",
        "    icon_toggle(ui, \"close\", false, \"Close panel\");\n",
        "}\n",
    );
    assert!(offending(now).is_empty());
}

#[test]
fn the_scanner_reads_strings_and_not_the_prose_about_them() {
    // Every way this could report the wrong thing, pinned. A symbol quoted in a
    // comment is documentation, not a label; a symbol in a char literal is a key
    // press; and a `//` inside a string is not the start of a comment.
    assert!(offending("// the old code typed \"\u{25B8}\" here\n").is_empty());
    assert!(offending("//! `\"\u{2715}\"` was the close button\n").is_empty());
    assert!(offending("/* \u{25D0} \u{270E} */\n").is_empty());
    assert!(offending("let k = tool_for_key('\u{00A7}');\n").is_empty());
    assert!(offending("let s = \"see https://example.com\u{2014}ok\";\n").is_empty());

    // An escaped code point is the character it stands for. Writing the
    // triangles this way is exactly how the Canvas Size dialog kept four of
    // them — two of which egui cannot draw — out of this scan.
    assert_eq!(offending("let s = \"\\u{25B2}\";\n"), vec![(1, '\u{25B2}')]);
    // ...and an escape of an allowlisted character is still allowed, and an
    // escaped quote still does not end the string.
    assert!(offending("let s = \"\\u{2014}\";\n").is_empty());
    assert!(offending("let s = \"a \\\" b\";\n").is_empty());

    // A raw string is still a string.
    assert_eq!(offending("let s = r#\"\u{25B8}\"#;\n").len(), 1);
    // A string on a line after a comment containing a quote is still found.
    let after = "// a \" quote in prose\nlet s = \"\u{2715}\";\n";
    assert_eq!(offending(after), vec![(2, '\u{2715}')]);
}
