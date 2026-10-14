//! The translation table (Prompt 25, ARCHITECTURE.md D134).
//!
//! Every visible string goes through [`tr`], keyed by its English text. With no table
//! installed (English, the only language so far) `tr` returns its argument. A table is plain
//! text, one `English = Translation` line per string (`#` starts a comment, `\n` is a line
//! break), so a translator needs no tools; [`install`] makes one current and [`template`]
//! lists every string a table can translate.

use std::collections::HashMap;
use std::sync::RwLock;

/// The languages offered: code and name.
pub const LANGUAGES: [(&str, &str); 1] = [("en", "English")];

type Table = HashMap<&'static str, &'static str>;

static CURRENT: RwLock<Option<&'static Table>> = RwLock::new(None);

/// `english` in the current language (itself when there is no translation).
pub fn tr(english: &'static str) -> &'static str {
    match CURRENT.read() {
        Ok(t) => lookup(*t, english),
        Err(_) => english,
    }
}

fn lookup(table: Option<&'static Table>, english: &'static str) -> &'static str {
    table
        .and_then(|t| t.get(english).copied())
        .unwrap_or(english)
}

/// Parses a table: `English = Translation` lines.
pub fn parse(text: &str) -> HashMap<String, String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| l.split_once(" = "))
        .map(|(a, b)| (unescape(a.trim()), unescape(b.trim())))
        .filter(|(a, b)| !a.is_empty() && !b.is_empty())
        .collect()
}

fn unescape(s: &str) -> String {
    s.replace("\\n", "\n")
}

/// Makes `table` (from [`parse`]) current; `None` goes back to English. Each table's strings
/// are kept for the rest of the run, so switching often leaks a little memory.
pub fn install(table: Option<HashMap<String, String>>) {
    let leaked = table.map(leak);
    if let Ok(mut c) = CURRENT.write() {
        *c = leaked;
    }
}

fn leak(t: HashMap<String, String>) -> &'static Table {
    let map: Table = t
        .into_iter()
        .map(|(a, b)| (&*a.leak(), &*b.leak()))
        .collect();
    Box::leak(Box::new(map))
}

/// Selects a language by code: English needs no table, others are bundled with the program.
/// Returns false for an unknown code (English stays).
pub fn set_language(code: &str) -> bool {
    match code {
        "en" | "" => {
            install(None);
            true
        }
        _ => false,
    }
}

/// A table template: every string `sources` passes to `tr("...")`, each as `text = `, for a
/// translator to fill in.
pub fn template<'a>(sources: impl IntoIterator<Item = &'a str>) -> String {
    let mut seen = std::collections::BTreeSet::new();
    for src in sources {
        let mut rest = src;
        while let Some(i) = rest.find("tr(\"") {
            rest = &rest[i + 4..];
            if let Some(end) = string_end(rest) {
                seen.insert(rest[..end].to_owned());
                rest = &rest[end..];
            }
        }
    }
    seen.into_iter()
        .map(|s| format!("{} = \n", s.replace('\n', "\\n")))
        .collect()
}

/// Length of a Rust string literal's body up to its closing quote.
fn string_end(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return Some(i),
            _ => i += 1,
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_translate_and_english_passes_through() {
        assert_eq!(tr("Save"), "Save");
        let t = parse(
            "# test language\nSave = Speichern\nOpen project = Projekt öffnen\n\
             broken line\nLine\\nbreak = Zeilen\\numbruch\n",
        );
        assert_eq!(t.len(), 3);
        // Looked up directly: installing would change other tests' strings mid-run.
        let t = Some(leak(t));
        assert_eq!(lookup(t, "Save"), "Speichern");
        assert_eq!(lookup(t, "Open project"), "Projekt öffnen");
        assert_eq!(lookup(t, "Line\nbreak"), "Zeilen\numbruch");
        assert_eq!(lookup(t, "Untranslated"), "Untranslated");
        assert!(set_language("en"));
        assert_eq!(tr("Save"), "Save");
        assert!(!set_language("xx"));
    }

    #[test]
    fn template_lists_each_string_once() {
        let src = r#"ui.label(tr("Save")); b(tr("Say \"hi\"")); c(tr("Save")); tr("Two\nlines")"#;
        assert_eq!(
            template([src]),
            "Save = \nSay \\\"hi\\\" = \nTwo\\nlines = \n"
        );
    }

    /// Every source file of the UI: this crate's views and the app's windows.
    fn ui_sources() -> Vec<(std::path::PathBuf, String)> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut out = Vec::new();
        let mut stack = vec![root.join("src"), root.join("../gt-app/src")];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    let text = std::fs::read_to_string(&p).unwrap();
                    // Test modules print nothing.
                    let cut = text.find("#[cfg(test)]\nmod ").unwrap_or(text.len());
                    out.push((p, text[..cut].to_owned()));
                }
            }
        }
        out
    }

    #[test]
    fn visible_strings_go_through_the_table() {
        let calls = [
            ".label(\"",
            "on_hover_text(\"",
            ".button(\"",
            "small_button(\"",
            "RichText::new(\"",
            "Button::new(\"",
            "Window::new(\"",
            "hint_text(\"",
            "menu_button(\"",
            "toast(\"",
            ".heading(\"",
        ];
        let mut bare = Vec::new();
        for (path, text) in ui_sources() {
            for call in calls {
                for (i, _) in text.match_indices(call) {
                    let lit = &text[i + call.len()..];
                    let body = &lit[..string_end(lit).unwrap_or(0)];
                    if body.chars().filter(char::is_ascii_alphabetic).count() >= 2 {
                        let line = text[..i].lines().count();
                        bare.push(format!("{}:{line}: {call}{body}", path.display()));
                    }
                }
            }
        }
        assert!(bare.is_empty(), "wrap these in tr():\n{}", bare.join("\n"));
        // The template a translator fills in covers the whole UI.
        let all = ui_sources();
        let t = template(all.iter().map(|(_, s)| s.as_str()));
        assert!(t.lines().count() > 400, "{}", t.lines().count());
        assert!(t.contains("Undo history = "));
    }
}
