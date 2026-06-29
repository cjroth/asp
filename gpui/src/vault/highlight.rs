//! Lightweight code syntax highlighting for fenced code blocks, inspired by the
//! per-language tokenizers in desktop `src/vault/markdown.ts` (simplified to a
//! generic scanner: comments, strings, numbers, keywords, types). Pure + tested.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tok {
    Plain,
    Keyword,
    Str,
    Comment,
    Number,
    Type,
}

/// Comment line-marker(s) for a language.
fn line_comment(lang: &str) -> &'static str {
    match lang {
        "py" | "python" | "sh" | "bash" | "zsh" | "yaml" | "yml" | "toml" | "rb" | "ruby" => "#",
        _ => "//",
    }
}

fn keywords(lang: &str) -> &'static [&'static str] {
    match lang {
        "rust" | "rs" => &[
            "fn", "let", "mut", "pub", "struct", "enum", "impl", "trait", "use", "mod", "match",
            "if", "else", "for", "while", "loop", "return", "self", "Self", "where", "as", "ref",
            "move", "async", "await", "const", "static", "dyn", "crate", "super", "in", "break",
            "continue", "type", "unsafe",
        ],
        "py" | "python" => &[
            "def", "class", "return", "if", "elif", "else", "for", "while", "import", "from", "as",
            "with", "try", "except", "finally", "raise", "lambda", "yield", "pass", "break",
            "continue", "and", "or", "not", "in", "is", "None", "True", "False", "async", "await",
        ],
        "go" => &[
            "func", "package", "import", "var", "const", "type", "struct", "interface", "map",
            "chan", "go", "defer", "return", "if", "else", "for", "range", "switch", "case",
            "default", "select", "break", "continue", "nil",
        ],
        _ => &[
            // js/ts and a reasonable default
            "function", "const", "let", "var", "return", "if", "else", "for", "while", "class",
            "extends", "import", "export", "from", "default", "async", "await", "new", "this",
            "typeof", "instanceof", "in", "of", "try", "catch", "finally", "throw", "switch",
            "case", "break", "continue", "interface", "type", "enum", "public", "private",
            "null", "undefined", "true", "false",
        ],
    }
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Tokenize one source line for `lang` into (kind, text) runs (covers the whole line).
pub fn highlight(line: &str, lang: &str) -> Vec<(Tok, String)> {
    let kws = keywords(lang);
    let cmt = line_comment(lang);
    let chars: Vec<char> = line.chars().collect();
    let mut out: Vec<(Tok, String)> = Vec::new();
    let mut plain = String::new();
    let flush = |plain: &mut String, out: &mut Vec<(Tok, String)>| {
        if !plain.is_empty() {
            out.push((Tok::Plain, std::mem::take(plain)));
        }
    };
    let mut i = 0;
    while i < chars.len() {
        // line comment → rest of line
        if chars[i..].iter().collect::<String>().starts_with(cmt) {
            flush(&mut plain, &mut out);
            out.push((Tok::Comment, chars[i..].iter().collect()));
            return out;
        }
        let c = chars[i];
        // string literal
        if c == '"' || c == '\'' || c == '`' {
            flush(&mut plain, &mut out);
            let quote = c;
            let start = i;
            i += 1;
            while i < chars.len() {
                if chars[i] == '\\' {
                    i += 2;
                    continue;
                }
                if chars[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push((Tok::Str, chars[start..i.min(chars.len())].iter().collect()));
            continue;
        }
        // number
        if c.is_ascii_digit() {
            flush(&mut plain, &mut out);
            let start = i;
            while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.' || chars[i] == '_') {
                i += 1;
            }
            out.push((Tok::Number, chars[start..i].iter().collect()));
            continue;
        }
        // identifier / keyword / type
        if is_ident(c) && !c.is_ascii_digit() {
            flush(&mut plain, &mut out);
            let start = i;
            while i < chars.len() && is_ident(chars[i]) {
                i += 1;
            }
            let word: String = chars[start..i].iter().collect();
            let kind = if kws.contains(&word.as_str()) {
                Tok::Keyword
            } else if word.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                Tok::Type
            } else {
                Tok::Plain
            };
            out.push((kind, word));
            continue;
        }
        plain.push(c);
        i += 1;
    }
    flush(&mut plain, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(line: &str, lang: &str) -> Vec<(Tok, String)> {
        highlight(line, lang)
    }

    #[test]
    fn rust_keywords_strings_comments() {
        let toks = kinds("fn main() { println!(\"hi\"); } // c", "rust");
        assert_eq!(toks[0], (Tok::Keyword, "fn".into()));
        assert!(toks.iter().any(|(k, t)| *k == Tok::Str && t == "\"hi\""));
        assert!(toks.iter().any(|(k, t)| *k == Tok::Comment && t.contains("// c")));
        // `main` is plain (a fn name), `println` plain too.
        assert!(toks.iter().any(|(k, t)| *k == Tok::Plain && t == "main"));
    }

    #[test]
    fn types_and_numbers() {
        let toks = kinds("let x: Vec<u8> = 42;", "rust");
        assert!(toks.iter().any(|(k, t)| *k == Tok::Keyword && t == "let"));
        assert!(toks.iter().any(|(k, t)| *k == Tok::Type && t == "Vec"));
        assert!(toks.iter().any(|(k, t)| *k == Tok::Number && t == "42"));
    }

    #[test]
    fn python_hash_comment() {
        let toks = kinds("x = 1  # note", "py");
        assert!(toks.iter().any(|(k, t)| *k == Tok::Comment && t.contains("# note")));
        assert!(toks.iter().any(|(k, t)| *k == Tok::Number && t == "1"));
    }

    #[test]
    fn covers_whole_line() {
        // concatenating token texts must reproduce the input exactly.
        let line = "const a = `tmpl` + 3.14; // x";
        let joined: String = highlight(line, "js").into_iter().map(|(_, t)| t).collect();
        assert_eq!(joined, line);
    }
}
