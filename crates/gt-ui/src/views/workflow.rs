//! Workflow windows (Prompt 25): the undo history, the project info and the help pages.

use egui::{RichText, Ui};
use gt_core::ProjectInfo;

use crate::i18n::tr;
use crate::keymap::{format_shortcut, Command, Keymap, Scope};
use crate::GloomTheme;

/// The undo history: one row per step, the current one highlighted, later (undone) steps
/// dimmed. Returns the number of steps to have applied when a row was clicked.
pub fn history_panel(
    ui: &mut Ui,
    theme: &GloomTheme,
    labels: &[&str],
    applied: usize,
) -> Option<usize> {
    let mut jump = None;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(360.0)
        .stick_to_bottom(true)
        .show(ui, |ui| {
            let rows = std::iter::once(tr("Opened")).chain(labels.iter().copied());
            for (i, label) in rows.enumerate() {
                let text = if i > applied {
                    RichText::new(label).color(theme.text_dim).italics()
                } else {
                    RichText::new(label)
                };
                if ui
                    .add(egui::Button::selectable(i == applied, text).truncate())
                    .clicked()
                    && i != applied
                {
                    jump = Some(i);
                }
            }
        });
    ui.label(
        RichText::new(tr("Click a step to go back or forward to it."))
            .small()
            .color(theme.text_dim),
    );
    jump
}

/// The project info form. Returns true when a field changed.
pub fn project_info_panel(ui: &mut Ui, theme: &GloomTheme, info: &mut ProjectInfo) -> bool {
    let mut changed = false;
    egui::Grid::new("project_info")
        .num_columns(2)
        .spacing([10.0, 6.0])
        .show(ui, |ui| {
            for (label, field) in [
                (tr("Title"), &mut info.title),
                (tr("Artist"), &mut info.artist),
                (tr("Genre"), &mut info.genre),
            ] {
                ui.label(RichText::new(label).color(theme.text_dim));
                changed |= ui
                    .add(egui::TextEdit::singleline(field).desired_width(280.0))
                    .changed();
                ui.end_row();
            }
        });
    ui.label(RichText::new(tr("Notes")).color(theme.text_dim));
    changed |= ui
        .add(
            egui::TextEdit::multiline(&mut info.notes)
                .desired_width(f32::INFINITY)
                .desired_rows(8),
        )
        .changed();
    changed
}

/// The bundled help pages: title and Markdown text.
pub const HELP_PAGES: [(&str, &str); 4] = [
    (
        "Getting started",
        include_str!("../../help/getting-started.md"),
    ),
    ("Browser", include_str!("../../help/browser.md")),
    ("Editing", include_str!("../../help/editing.md")),
    ("Mixing and export", include_str!("../../help/mixing.md")),
];

/// The help window's state: the open page (`HELP_PAGES.len()` is the shortcut list).
#[derive(Debug, Clone, Default)]
pub struct HelpState {
    /// Open page.
    pub page: usize,
}

/// The help window: a page list and the open page.
pub fn help_panel(ui: &mut Ui, theme: &GloomTheme, state: &mut HelpState, keymap: &Keymap) {
    ui.horizontal_wrapped(|ui| {
        for (i, (title, _)) in HELP_PAGES.iter().enumerate() {
            if ui
                .add(egui::Button::selectable(state.page == i, tr(title)))
                .clicked()
            {
                state.page = i;
            }
        }
        let keys = HELP_PAGES.len();
        if ui
            .add(egui::Button::selectable(
                state.page == keys,
                tr("Keyboard shortcuts"),
            ))
            .clicked()
        {
            state.page = keys;
        }
    });
    ui.separator();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| match HELP_PAGES.get(state.page) {
            Some((_, text)) => markdown(ui, theme, text),
            None => markdown(ui, theme, &shortcut_page(keymap)),
        });
}

/// The shortcut list as Markdown, from the user's keymap.
pub fn shortcut_page(keymap: &Keymap) -> String {
    let mut out = format!("# {}\n", tr("Keyboard shortcuts"));
    for scope in Scope::ALL {
        out.push_str(&format!("\n## {}\n\n", scope.label()));
        for c in Command::ALL.into_iter().filter(|c| c.scope() == scope) {
            let keys: Vec<String> = keymap.get(c).iter().map(format_shortcut).collect();
            if !keys.is_empty() {
                out.push_str(&format!("- **{}**: {}\n", keys.join(", "), c.label()));
            }
        }
    }
    out
}

/// One block of a Markdown page.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// A heading and its level (1 to 3).
    Heading(u8, String),
    /// A paragraph (its lines joined).
    Paragraph(String),
    /// A bullet ("-") or numbered ("1.") list item and its marker.
    Item(String, String),
}

/// Splits the Markdown subset the help pages use into blocks: `#` headings, paragraphs,
/// `-` and `1.` list items (continued by indented lines). Inline `**bold**`, `*italic*` and
/// `` `code` `` stay in the text for [`inline`].
pub fn blocks(text: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let flush = |para: &mut Vec<&str>, out: &mut Vec<Block>| {
        if !para.is_empty() {
            out.push(Block::Paragraph(para.join(" ")));
            para.clear();
        }
    };
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            flush(&mut para, &mut out);
            continue;
        }
        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        if (1..=3).contains(&hashes) && trimmed[hashes..].starts_with(' ') {
            flush(&mut para, &mut out);
            out.push(Block::Heading(
                hashes as u8,
                trimmed[hashes..].trim().to_owned(),
            ));
            continue;
        }
        let numbered = trimmed
            .split_once(". ")
            .filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if let Some(rest) = trimmed.strip_prefix("- ") {
            flush(&mut para, &mut out);
            out.push(Block::Item("•".to_owned(), rest.to_owned()));
        } else if let Some((n, rest)) = numbered {
            flush(&mut para, &mut out);
            out.push(Block::Item(format!("{n}."), rest.to_owned()));
        } else if line.starts_with("  ") && para.is_empty() {
            // Continues the list item above.
            if let Some(Block::Item(_, t)) = out.last_mut() {
                t.push(' ');
                t.push_str(trimmed);
            } else {
                para.push(trimmed);
            }
        } else {
            para.push(trimmed);
        }
    }
    flush(&mut para, &mut out);
    out
}

/// A run of text and its style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Span<'a> {
    /// Plain text.
    Plain(&'a str),
    /// `**bold**`.
    Bold(&'a str),
    /// `*italic*`.
    Italic(&'a str),
    /// `` `code` ``.
    Code(&'a str),
}

/// Splits a line into styled runs. An unclosed marker is kept as text.
pub fn inline(text: &str) -> Vec<Span<'_>> {
    let mut out = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let next = ["**", "*", "`"]
            .iter()
            .filter_map(|m| rest.find(m).map(|i| (i, *m)))
            .min_by_key(|&(i, m)| (i, std::cmp::Reverse(m.len())));
        let Some((i, marker)) = next else {
            out.push(Span::Plain(rest));
            break;
        };
        let after = &rest[i + marker.len()..];
        let Some(end) = after.find(marker).filter(|&e| e > 0) else {
            // No closing marker: plain up to and including this one.
            out.push(Span::Plain(&rest[..i + marker.len()]));
            rest = after;
            continue;
        };
        if i > 0 {
            out.push(Span::Plain(&rest[..i]));
        }
        let body = &after[..end];
        out.push(match marker {
            "**" => Span::Bold(body),
            "*" => Span::Italic(body),
            _ => Span::Code(body),
        });
        rest = &after[end + marker.len()..];
    }
    out
}

/// Draws a Markdown page.
pub fn markdown(ui: &mut Ui, theme: &GloomTheme, text: &str) {
    for block in blocks(text) {
        match block {
            Block::Heading(level, t) => {
                ui.add_space(if level == 1 { 2.0 } else { 8.0 });
                let size = theme.font_size + [10.0, 5.0, 2.0][usize::from(level - 1).min(2)];
                ui.label(RichText::new(t).size(size).strong().color(theme.text));
            }
            Block::Paragraph(t) => {
                rich_line(ui, theme, &t);
                ui.add_space(4.0);
            }
            Block::Item(marker, t) => {
                ui.horizontal_top(|ui| {
                    ui.add_sized(
                        [18.0, ui.spacing().interact_size.y * 0.8],
                        egui::Label::new(RichText::new(marker).color(theme.accent)),
                    );
                    ui.vertical(|ui| rich_line(ui, theme, &t));
                });
            }
        }
    }
}

fn rich_line(ui: &mut Ui, theme: &GloomTheme, text: &str) {
    let mut job = egui::text::LayoutJob::default();
    let font = egui::FontId::proportional(theme.font_size);
    let mono = egui::FontId::monospace(theme.font_size - 1.0);
    for span in inline(text) {
        let (t, format) = match span {
            Span::Plain(t) => (t, egui::TextFormat::simple(font.clone(), theme.text)),
            Span::Bold(t) => (t, egui::TextFormat::simple(font.clone(), theme.accent)),
            Span::Italic(t) => (
                t,
                egui::TextFormat {
                    italics: true,
                    ..egui::TextFormat::simple(font.clone(), theme.text)
                },
            ),
            Span::Code(t) => (
                t,
                egui::TextFormat {
                    background: theme.bg_widget,
                    ..egui::TextFormat::simple(mono.clone(), theme.text)
                },
            ),
        };
        job.append(t, 0.0, format);
    }
    job.wrap.max_width = ui.available_width();
    ui.label(job);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_blocks_and_spans() {
        let page = "# Title\n\nFirst line\nsecond line.\n\n## Part\n\n- one\n  more\n- two\n\
                    1. first\n2. second\n";
        assert_eq!(
            blocks(page),
            vec![
                Block::Heading(1, "Title".into()),
                Block::Paragraph("First line second line.".into()),
                Block::Heading(2, "Part".into()),
                Block::Item("•".into(), "one more".into()),
                Block::Item("•".into(), "two".into()),
                Block::Item("1.".into(), "first".into()),
                Block::Item("2.".into(), "second".into()),
            ]
        );
        assert_eq!(
            inline("Press **F6**, then *play* `Space` a*b"),
            vec![
                Span::Plain("Press "),
                Span::Bold("F6"),
                Span::Plain(", then "),
                Span::Italic("play"),
                Span::Plain(" "),
                Span::Code("Space"),
                Span::Plain(" a*"),
                Span::Plain("b"),
            ]
        );
    }

    #[test]
    fn bundled_pages_parse_and_shortcuts_follow_the_keymap() {
        for (title, text) in HELP_PAGES {
            let b = blocks(text);
            assert!(matches!(b.first(), Some(Block::Heading(1, _))), "{title}");
            assert!(b.len() > 5, "{title}");
        }
        let mut keys = Keymap::default();
        let page = shortcut_page(&keys);
        assert!(page.contains("- **Space**: Play / pause"));
        assert!(page.contains("## Piano roll"));
        keys.clear(Command::PlayPause);
        assert!(!shortcut_page(&keys).contains("Play / pause"));
    }
}
