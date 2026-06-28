//! Minimal read-only "live" Markdown renderer → GPUI elements. Ports the spirit
//! of the design's `markdown.ts`: headings, bullet/task/ordered lists,
//! blockquotes, fenced code, horizontal rules, and inline emphasis/code/links.

use crate::theme::Theme;
use gpui::prelude::*;
use gpui::{div, px, AnyElement, FontStyle, FontWeight, HighlightStyle, SharedString, StyledText, UnderlineStyle};

const SERIF: &str = "Newsreader";
const MONO: &str = "JetBrains Mono";

/// Render a code/plain file as one monospace block.
pub fn render_code(src: &str, t: &Theme) -> AnyElement {
    div()
        .font_family(MONO)
        .text_size(px(13.))
        .text_color(t.text)
        .child(src.to_string())
        .into_any_element()
}

/// Render Markdown source into a vertical stack of styled blocks.
pub fn render_markdown(src: &str, t: &Theme) -> AnyElement {
    let mut blocks: Vec<AnyElement> = Vec::new();
    let lines: Vec<&str> = src.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();

        // Fenced code block.
        if trimmed.starts_with("```") {
            let mut code = String::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                code.push_str(lines[i]);
                code.push('\n');
                i += 1;
            }
            i += 1; // closing fence
            blocks.push(
                div()
                    .my(px(8.))
                    .px(px(14.))
                    .py(px(12.))
                    .bg(t.bg_input)
                    .border_1()
                    .border_color(t.line)
                    .rounded(px(9.))
                    .font_family(MONO)
                    .text_size(px(13.))
                    .text_color(t.text2)
                    .child(code.trim_end().to_string())
                    .into_any_element(),
            );
            continue;
        }

        // Horizontal rule.
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            blocks.push(div().my(px(12.)).h(px(1.)).w_full().bg(t.line).into_any_element());
            i += 1;
            continue;
        }

        // Blank line → spacer.
        if trimmed.is_empty() {
            blocks.push(div().h(px(10.)).into_any_element());
            i += 1;
            continue;
        }

        // Headings.
        if let Some(rest) = trimmed.strip_prefix("### ") {
            blocks.push(heading(rest, 18., t));
            i += 1;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("## ") {
            blocks.push(heading(rest, 21., t));
            i += 1;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("# ") {
            blocks.push(heading(rest, 26., t));
            i += 1;
            continue;
        }

        // Blockquote.
        if let Some(rest) = trimmed.strip_prefix("> ") {
            blocks.push(
                div()
                    .pl(px(14.))
                    .py(px(1.))
                    .border_l_3()
                    .border_color(t.accent_soft())
                    .text_color(t.text3)
                    .italic()
                    .child(inline(rest, t))
                    .into_any_element(),
            );
            i += 1;
            continue;
        }

        // Task list item.
        if let Some(rest) = task_item(trimmed) {
            let (done, text) = rest;
            blocks.push(
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(8.))
                    .child(
                        div()
                            .mt(px(4.))
                            .w(px(15.))
                            .h(px(15.))
                            .rounded(px(4.))
                            .border_2()
                            .border_color(if done { t.accent } else { t.faint2 })
                            .when(done, |d| d.bg(t.accent))
                            .flex()
                            .items_center()
                            .justify_center()
                            .when(done, |d| {
                                d.text_color(t.bg).text_size(px(10.)).child("✓")
                            }),
                    )
                    .child(div().flex_1().child(inline(text, t)))
                    .into_any_element(),
            );
            i += 1;
            continue;
        }

        // Bullet list item.
        if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            blocks.push(bullet_row("•", rest, t));
            i += 1;
            continue;
        }

        // Ordered list item ("1. ").
        if let Some((num, rest)) = ordered_item(trimmed) {
            blocks.push(bullet_row(&format!("{num}."), rest, t));
            i += 1;
            continue;
        }

        // Paragraph.
        blocks.push(
            div()
                .py(px(2.))
                .text_color(t.text)
                .child(inline(trimmed, t))
                .into_any_element(),
        );
        i += 1;
    }

    div()
        .flex()
        .flex_col()
        .font_family(SERIF)
        .text_size(px(16.))
        .line_height(px(28.))
        .text_color(t.text)
        .children(blocks)
        .into_any_element()
}

fn heading(text: &str, size: f32, t: &Theme) -> AnyElement {
    div()
        .mt(px(10.))
        .mb(px(2.))
        .text_size(px(size))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.text)
        .child(inline(text, t))
        .into_any_element()
}

fn bullet_row(marker: &str, text: &str, t: &Theme) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .items_start()
        .gap(px(8.))
        .child(
            div()
                .w(px(16.))
                .flex_shrink_0()
                .text_color(t.accent)
                .font_weight(FontWeight::BOLD)
                .child(marker.to_string()),
        )
        .child(div().flex_1().text_color(t.text).child(inline(text, t)))
        .into_any_element()
}

fn task_item(line: &str) -> Option<(bool, &str)> {
    if let Some(rest) = line.strip_prefix("- [ ] ") {
        Some((false, rest))
    } else if let Some(rest) = line.strip_prefix("- [x] ").or_else(|| line.strip_prefix("- [X] ")) {
        Some((true, rest))
    } else {
        None
    }
}

fn ordered_item(line: &str) -> Option<(u32, &str)> {
    let dot = line.find(". ")?;
    let num: u32 = line[..dot].parse().ok()?;
    Some((num, &line[dot + 2..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_items() {
        assert_eq!(task_item("- [ ] todo"), Some((false, "todo")));
        assert_eq!(task_item("- [x] done"), Some((true, "done")));
        assert_eq!(task_item("- [X] done"), Some((true, "done")));
        assert_eq!(task_item("- plain"), None);
    }

    #[test]
    fn ordered_items() {
        assert_eq!(ordered_item("1. first"), Some((1, "first")));
        assert_eq!(ordered_item("42. answer"), Some((42, "answer")));
        assert_eq!(ordered_item("- bullet"), None);
        assert_eq!(ordered_item("no number"), None);
    }
}

/// Parse inline markers (**bold**, *italic*, `code`, [text](url)) into a
/// `StyledText` whose runs override the surrounding block's base text style.
fn inline(src: &str, t: &Theme) -> StyledText {
    let mut out = String::new();
    let mut runs: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();
    let bytes = src.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        // Bold **...**
        if src[i..].starts_with("**") {
            if let Some(end) = src[i + 2..].find("**") {
                let inner = &src[i + 2..i + 2 + end];
                let start = out.len();
                out.push_str(inner);
                runs.push((
                    start..out.len(),
                    HighlightStyle {
                        font_weight: Some(FontWeight::BOLD),
                        ..Default::default()
                    },
                ));
                i = i + 2 + end + 2;
                continue;
            }
        }
        // Inline code `...`
        if bytes[i] == b'`' {
            if let Some(end) = src[i + 1..].find('`') {
                let inner = &src[i + 1..i + 1 + end];
                let start = out.len();
                out.push_str(inner);
                runs.push((
                    start..out.len(),
                    HighlightStyle {
                        color: Some(t.text2.into()),
                        background_color: Some(t.bg_input.into()),
                        ..Default::default()
                    },
                ));
                i = i + 1 + end + 1;
                continue;
            }
        }
        // Italic *...* (single star, not part of **)
        if bytes[i] == b'*' && !src[i..].starts_with("**") {
            if let Some(end) = src[i + 1..].find('*') {
                let inner = &src[i + 1..i + 1 + end];
                let start = out.len();
                out.push_str(inner);
                runs.push((
                    start..out.len(),
                    HighlightStyle {
                        font_style: Some(FontStyle::Italic),
                        ..Default::default()
                    },
                ));
                i = i + 1 + end + 1;
                continue;
            }
        }
        // Link [text](url)
        if bytes[i] == b'[' {
            if let Some(close) = src[i..].find("](") {
                if let Some(paren) = src[i + close + 2..].find(')') {
                    let text = &src[i + 1..i + close];
                    let start = out.len();
                    out.push_str(text);
                    runs.push((
                        start..out.len(),
                        HighlightStyle {
                            color: Some(t.accent.into()),
                            underline: Some(UnderlineStyle {
                                thickness: px(1.),
                                color: Some(t.accent.into()),
                                wavy: false,
                            }),
                            ..Default::default()
                        },
                    ));
                    i = i + close + 2 + paren + 1;
                    continue;
                }
            }
        }

        // Plain char (advance by full UTF-8 char).
        let ch = src[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }

    StyledText::new(SharedString::from(out)).with_highlights(runs)
}
