//! Minimal mermaid flowchart parsing for native rendering. Full mermaid (its
//! layout engine + all diagram types) needs the mermaid.js runtime, which can't
//! run in pure Rust/gpui — but the common `graph TD; A[Foo] --> B[Bar]` flowchart
//! syntax parses cleanly into nodes + edges that we render as boxes + arrows.
//! Pure + tested; the gpui rendering lives in `screens::editor`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    /// `graph TD` / `flowchart LR` header.
    Directive(String),
    /// A standalone node declaration `id[Label]`.
    Node { label: String },
    /// An edge `A[Foo] --> B[Bar]` (optionally `-->|label|`).
    Edge { from: String, to: String, label: Option<String> },
    /// Anything we don't model — shown as raw source.
    Raw(String),
}

const ARROWS: [&str; 5] = ["-->", "---", "-.->", "==>", "--"];

/// Strip a node's `id[Label]` / `id(Label)` / `id{Label}` to its display label
/// (the bracketed text), or the bare id if undecorated.
fn node_label(part: &str) -> String {
    let p = part.trim();
    for (open, close) in [('[', ']'), ('(', ')'), ('{', '}')] {
        if let Some(o) = p.find(open) {
            if let Some(c) = p.rfind(close) {
                if c > o {
                    return p[o + 1..c].trim().trim_matches('"').to_string();
                }
            }
        }
    }
    p.to_string()
}

/// Parse one mermaid line.
pub fn parse_line(line: &str) -> Item {
    let t = line.trim();
    if t.is_empty() {
        return Item::Raw(String::new());
    }
    let low = t.to_lowercase();
    if low.starts_with("graph ") || low.starts_with("flowchart ") || low == "graph" || low == "flowchart" {
        return Item::Directive(t.to_string());
    }
    // edge: split on the first arrow token that appears.
    for arrow in ARROWS {
        if let Some(idx) = t.find(arrow) {
            let left = &t[..idx];
            let mut right = &t[idx + arrow.len()..];
            // optional `|edge label|` immediately after the arrow
            let mut edge_label = None;
            let rt = right.trim_start();
            if let Some(stripped) = rt.strip_prefix('|') {
                if let Some(end) = stripped.find('|') {
                    edge_label = Some(stripped[..end].trim().to_string());
                    right = &stripped[end + 1..];
                }
            }
            return Item::Edge {
                from: node_label(left),
                to: node_label(right),
                label: edge_label.filter(|s| !s.is_empty()),
            };
        }
    }
    // node-only declaration (has brackets)?
    if t.contains('[') || t.contains('(') || t.contains('{') {
        return Item::Node { label: node_label(t) };
    }
    Item::Raw(t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive() {
        assert_eq!(parse_line("graph TD"), Item::Directive("graph TD".into()));
        assert_eq!(parse_line("flowchart LR"), Item::Directive("flowchart LR".into()));
    }

    #[test]
    fn simple_edge() {
        assert_eq!(
            parse_line("A --> B"),
            Item::Edge { from: "A".into(), to: "B".into(), label: None }
        );
        assert_eq!(
            parse_line("A-->B"),
            Item::Edge { from: "A".into(), to: "B".into(), label: None }
        );
    }

    #[test]
    fn labeled_nodes_and_edge() {
        assert_eq!(
            parse_line("Start[Begin here] --> Mid(process)"),
            Item::Edge { from: "Begin here".into(), to: "process".into(), label: None }
        );
        assert_eq!(
            parse_line("A -->|yes| B[Done]"),
            Item::Edge { from: "A".into(), to: "Done".into(), label: Some("yes".into()) }
        );
    }

    #[test]
    fn standalone_node() {
        assert_eq!(parse_line("X[A node]"), Item::Node { label: "A node".into() });
    }

    #[test]
    fn raw_fallback() {
        assert_eq!(parse_line("subgraph cluster"), Item::Raw("subgraph cluster".into()));
    }
}
