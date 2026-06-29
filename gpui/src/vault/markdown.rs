//! Markdown → render model, ported from desktop `src/vault/markdown.ts`. Keeps
//! the design's strict ONE-BLOCK-PER-SOURCE-LINE model (blank line → spacer).
//! This is the parse/structure half; `screens::editor` renders `Line`s to gpui
//! elements with the per-block styling from DESIGN_SPEC §5.3. Per-language code
//! syntax highlighting is intentionally deferred (code renders monospace).

/// One inline run within a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    Text(String),
    Bold(String),
    Italic(String),
    Code(String),
    Link { text: String, url: String },
    Image { alt: String, url: String },
}

/// One source line's rendered form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    Blank,
    Heading { level: u8, spans: Vec<Inline> },
    Quote(Vec<Inline>),
    Hr,
    Task { indent: usize, done: bool, spans: Vec<Inline> },
    Bullet { indent: usize, spans: Vec<Inline> },
    Ordered { indent: usize, number: String, spans: Vec<Inline> },
    Code(String),
    Para(Vec<Inline>),
}

/// Heading font sizes by level (1..=4), from the design.
pub fn heading_size(level: u8) -> f32 {
    match level {
        1 => 26.0,
        2 => 21.0,
        3 => 17.5,
        _ => 15.5,
    }
}

fn leading_spaces(s: &str) -> usize {
    s.chars().take_while(|c| *c == ' ' || *c == '\t').count()
}

/// Parse markdown source into one `Line` per source line (code fences emit their
/// content + fence markers as `Code` lines).
pub fn parse(src: &str) -> Vec<Line> {
    let mut out = Vec::new();
    let mut in_fence = false;
    for ln in src.split('\n') {
        if ln.trim_start().starts_with("```") {
            in_fence = !in_fence;
            out.push(Line::Code(ln.to_string()));
            continue;
        }
        if in_fence {
            out.push(Line::Code(ln.to_string()));
            continue;
        }
        if ln.is_empty() {
            out.push(Line::Blank);
            continue;
        }
        // Heading: #{1,4} + space + rest
        if let Some(h) = parse_heading(ln) {
            out.push(h);
            continue;
        }
        // Blockquote: > rest
        if let Some(rest) = ln.strip_prefix('>') {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            out.push(Line::Quote(parse_inline(rest)));
            continue;
        }
        // Horizontal rule: ---+ or ***+
        let t = ln.trim_end();
        if (t.len() >= 3 && t.chars().all(|c| c == '-')) || (t.len() >= 3 && t.chars().all(|c| c == '*')) {
            out.push(Line::Hr);
            continue;
        }
        // Task / bullet / ordered (with leading indent).
        if let Some(l) = parse_list(ln) {
            out.push(l);
            continue;
        }
        out.push(Line::Para(parse_inline(ln)));
    }
    out
}

fn parse_heading(ln: &str) -> Option<Line> {
    let hashes = ln.chars().take_while(|c| *c == '#').count();
    if hashes == 0 || hashes > 4 {
        return None;
    }
    let rest = &ln[hashes..];
    // require at least one space after the hashes
    if !rest.starts_with(' ') {
        return None;
    }
    let content = rest.trim_start_matches(' ');
    Some(Line::Heading { level: hashes as u8, spans: parse_inline(content) })
}

fn parse_list(ln: &str) -> Option<Line> {
    let indent = leading_spaces(ln);
    let body = &ln[indent..];
    // bullet marker: - or *
    if let Some(after) = body.strip_prefix("- ").or_else(|| body.strip_prefix("* ")) {
        // task? [ ] or [x]/[X] then space
        if (after.starts_with("[ ]") || after.starts_with("[x]") || after.starts_with("[X]"))
            && after[3..].starts_with(' ')
        {
            let done = after.starts_with("[x]") || after.starts_with("[X]");
            let content = after[3..].trim_start_matches(' ');
            return Some(Line::Task { indent, done, spans: parse_inline(content) });
        }
        return Some(Line::Bullet { indent, spans: parse_inline(after) });
    }
    // ordered: <digits>. <space>
    let digits: String = body.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        let after_digits = &body[digits.len()..];
        if let Some(after) = after_digits.strip_prefix(". ") {
            return Some(Line::Ordered {
                indent,
                number: format!("{digits}."),
                spans: parse_inline(after),
            });
        }
    }
    None
}

/// Inline parser: code, images, links, bold, italic — in that precedence,
/// mirroring `inlineMd`'s ordered passes. Falls back to plain text.
pub fn parse_inline(raw: &str) -> Vec<Inline> {
    let chars: Vec<char> = raw.chars().collect();
    let mut out: Vec<Inline> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;

    let flush = |buf: &mut String, out: &mut Vec<Inline>| {
        if !buf.is_empty() {
            out.push(Inline::Text(std::mem::take(buf)));
        }
    };

    while i < chars.len() {
        let c = chars[i];
        // inline code `...`
        if c == '`' {
            if let Some(end) = find(&chars, i + 1, '`') {
                flush(&mut buf, &mut out);
                out.push(Inline::Code(chars[i + 1..end].iter().collect()));
                i = end + 1;
                continue;
            }
        }
        // image ![alt](url)
        if c == '!' && i + 1 < chars.len() && chars[i + 1] == '[' {
            if let Some((alt, url, next)) = parse_link_like(&chars, i + 1) {
                flush(&mut buf, &mut out);
                out.push(Inline::Image { alt, url });
                i = next;
                continue;
            }
        }
        // link [text](url)
        if c == '[' {
            if let Some((text, url, next)) = parse_link_like(&chars, i) {
                flush(&mut buf, &mut out);
                out.push(Inline::Link { text, url });
                i = next;
                continue;
            }
        }
        // bold **...**
        if c == '*' && i + 1 < chars.len() && chars[i + 1] == '*' {
            if let Some(end) = find_seq(&chars, i + 2, '*', '*') {
                flush(&mut buf, &mut out);
                out.push(Inline::Bold(chars[i + 2..end].iter().collect()));
                i = end + 2;
                continue;
            }
        }
        // italic *...* (not preceded by a word char, content has no '*')
        if c == '*' {
            let prev_ok = i == 0 || {
                let p = chars[i - 1];
                p != '*' && !p.is_alphanumeric()
            };
            if prev_ok {
                if let Some(end) = find_italic_close(&chars, i + 1) {
                    flush(&mut buf, &mut out);
                    out.push(Inline::Italic(chars[i + 1..end].iter().collect()));
                    i = end + 1;
                    continue;
                }
            }
        }
        buf.push(c);
        i += 1;
    }
    flush(&mut buf, &mut out);
    out
}

fn find(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len()).find(|&j| chars[j] == target)
}

fn find_seq(chars: &[char], from: usize, a: char, b: char) -> Option<usize> {
    let mut j = from;
    while j + 1 < chars.len() {
        if chars[j] == a && chars[j + 1] == b {
            // non-empty content
            if j > from {
                return Some(j);
            }
        }
        j += 1;
    }
    None
}

fn find_italic_close(chars: &[char], from: usize) -> Option<usize> {
    if from >= chars.len() || chars[from] == '*' {
        return None;
    }
    for j in from..chars.len() {
        if chars[j] == '\n' {
            return None;
        }
        if chars[j] == '*' && j > from {
            return Some(j);
        }
    }
    None
}

/// Parse `[text](url)` starting at `chars[open]` == '['. Returns (text, url, next).
fn parse_link_like(chars: &[char], open: usize) -> Option<(String, String, usize)> {
    if chars.get(open) != Some(&'[') {
        return None;
    }
    let close = find(chars, open + 1, ']')?;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let url_end = find(chars, close + 2, ')')?;
    let text: String = chars[open + 1..close].iter().collect();
    let url: String = chars[close + 2..url_end].iter().collect();
    Some((text, url, url_end + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Vec<Inline> {
        parse_inline(s)
    }

    #[test]
    fn inline_plain_text() {
        assert_eq!(p("hello world"), vec![Inline::Text("hello world".into())]);
    }

    #[test]
    fn inline_bold_italic_code() {
        assert_eq!(
            p("a **b** c"),
            vec![Inline::Text("a ".into()), Inline::Bold("b".into()), Inline::Text(" c".into())]
        );
        assert_eq!(
            p("a *b* c"),
            vec![Inline::Text("a ".into()), Inline::Italic("b".into()), Inline::Text(" c".into())]
        );
        assert_eq!(
            p("use `code` here"),
            vec![Inline::Text("use ".into()), Inline::Code("code".into()), Inline::Text(" here".into())]
        );
    }

    #[test]
    fn inline_does_not_italicize_midword_asterisk() {
        // `a*b*` — '*' preceded by word char → not italic (stays literal text).
        assert_eq!(p("a*b*"), vec![Inline::Text("a*b*".into())]);
    }

    #[test]
    fn inline_link_and_image() {
        assert_eq!(
            p("see [docs](https://x.io)!"),
            vec![
                Inline::Text("see ".into()),
                Inline::Link { text: "docs".into(), url: "https://x.io".into() },
                Inline::Text("!".into()),
            ]
        );
        assert_eq!(
            p("![logo](a.png)"),
            vec![Inline::Image { alt: "logo".into(), url: "a.png".into() }]
        );
    }

    #[test]
    fn blocks_headings_and_para() {
        let ls = parse("# Title\n\nbody text");
        assert_eq!(ls[0], Line::Heading { level: 1, spans: vec![Inline::Text("Title".into())] });
        assert_eq!(ls[1], Line::Blank);
        assert_eq!(ls[2], Line::Para(vec![Inline::Text("body text".into())]));
    }

    #[test]
    fn blocks_heading_levels_need_space() {
        // "####title" (no space) is not a heading.
        assert_eq!(parse("####title")[0], Line::Para(vec![Inline::Text("####title".into())]));
        assert!(matches!(parse("### Three")[0], Line::Heading { level: 3, .. }));
        // 5 hashes is too deep → paragraph.
        assert!(matches!(parse("##### Five")[0], Line::Para(_)));
    }

    #[test]
    fn blocks_lists_tasks_ordered() {
        assert_eq!(
            parse("- item")[0],
            Line::Bullet { indent: 0, spans: vec![Inline::Text("item".into())] }
        );
        assert_eq!(
            parse("  - nested")[0],
            Line::Bullet { indent: 2, spans: vec![Inline::Text("nested".into())] }
        );
        assert_eq!(
            parse("- [ ] todo")[0],
            Line::Task { indent: 0, done: false, spans: vec![Inline::Text("todo".into())] }
        );
        assert_eq!(
            parse("- [x] done")[0],
            Line::Task { indent: 0, done: true, spans: vec![Inline::Text("done".into())] }
        );
        assert_eq!(
            parse("3. third")[0],
            Line::Ordered { indent: 0, number: "3.".into(), spans: vec![Inline::Text("third".into())] }
        );
    }

    #[test]
    fn blocks_quote_hr_code_fence() {
        assert_eq!(parse("> quoted")[0], Line::Quote(vec![Inline::Text("quoted".into())]));
        assert_eq!(parse("---")[0], Line::Hr);
        assert_eq!(parse("***")[0], Line::Hr);
        let fence = parse("```rust\nlet x = 1;\n```");
        assert_eq!(fence[0], Line::Code("```rust".into()));
        assert_eq!(fence[1], Line::Code("let x = 1;".into()));
        assert_eq!(fence[2], Line::Code("```".into()));
    }

    #[test]
    fn fence_suppresses_block_parsing_inside() {
        // a '#' inside a fence is code, not a heading.
        let ls = parse("```\n# not a heading\n```");
        assert_eq!(ls[1], Line::Code("# not a heading".into()));
    }
}
