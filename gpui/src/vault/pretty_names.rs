//! "Pretty filenames" + hidden-file helpers, ported 1:1 from desktop
//! `src/vault/prettyNames.ts`. Pure string transforms.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrettyLabel {
    pub label: String,
    pub italic: bool,
}

pub fn is_hidden(name: &str) -> bool {
    name.starts_with('.')
}

/// Mirror JS `s.split(/\s+/)`: split on maximal whitespace runs, preserving
/// leading/trailing empty segments (which `str::split_whitespace` would drop).
fn js_split_ws(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut prev_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_ws {
                parts.push(std::mem::take(&mut cur));
            }
            prev_ws = true;
        } else {
            cur.push(c);
            prev_ws = false;
        }
    }
    parts.push(cur);
    parts
}

fn titleize(s: &str) -> String {
    js_split_ws(s)
        .iter()
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Collapse runs of `-`/`_` into a single space (JS `replace(/[-_]+/g, ' ')`).
fn dashes_to_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_sep = false;
    for c in s.chars() {
        if c == '-' || c == '_' {
            if !prev_sep {
                out.push(' ');
            }
            prev_sep = true;
        } else {
            out.push(c);
            prev_sep = false;
        }
    }
    out
}

/// Turn a raw filename into a human label. Dotfiles verbatim; dirs/notes
/// titleized; an ALL-CAPS note stem is flagged italic.
pub fn pretty_name(name: &str, is_dir: bool) -> PrettyLabel {
    if name.starts_with('.') {
        return PrettyLabel { label: name.to_string(), italic: false };
    }
    if is_dir {
        return PrettyLabel { label: titleize(&dashes_to_spaces(name)), italic: false };
    }
    // /\.md$/i
    let lower = name.to_lowercase();
    if let Some(stripped) = lower.strip_suffix(".md") {
        let base = &name[..stripped.len()];
        let all_caps = !base.is_empty()
            && base.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            && base.chars().any(|c| c.is_ascii_uppercase());
        return PrettyLabel {
            label: titleize(&dashes_to_spaces(base)),
            italic: all_caps,
        };
    }
    PrettyLabel { label: name.to_string(), italic: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lbl(label: &str, italic: bool) -> PrettyLabel {
        PrettyLabel { label: label.into(), italic }
    }

    #[test]
    fn detects_hidden() {
        assert!(is_hidden(".gitignore"));
        assert!(!is_hidden("README.md"));
    }

    #[test]
    fn dotfiles_verbatim() {
        assert_eq!(pretty_name(".gitignore", false), lbl(".gitignore", false));
    }

    #[test]
    fn titleizes_directories() {
        assert_eq!(pretty_name("my-notes_folder", true), lbl("My Notes Folder", false));
    }

    #[test]
    fn titleizes_notes_and_flags_caps_italic() {
        assert_eq!(pretty_name("quick-thoughts.md", false), lbl("Quick Thoughts", false));
        assert_eq!(pretty_name("README.md", false), lbl("Readme", true));
        assert_eq!(pretty_name("TODO.md", false), lbl("Todo", true));
    }

    #[test]
    fn leaves_non_markdown_alone() {
        assert_eq!(pretty_name("sync.ts", false), lbl("sync.ts", false));
    }

    #[test]
    fn handles_leading_separators() {
        assert_eq!(pretty_name("-drafts", true), lbl(" Drafts", false));
    }
}
