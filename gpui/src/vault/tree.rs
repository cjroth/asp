//! Nested file-tree construction + flattening, ported 1:1 from desktop
//! `src/vault/tree.ts`. Build a nested tree from the backend's flat file list
//! (slash paths), then flatten to indented rows honoring an `expanded` map.
//! Directories arrive both as explicit `is_dir` entries and as implied parents
//! of file paths — we union both.

use std::cmp::Ordering;
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeKind {
    File,
    Dir,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeNode {
    pub kind: NodeKind,
    pub name: String,
    pub path: String,
    pub children: Vec<TreeNode>,
}

impl TreeNode {
    fn dir(name: &str, path: &str) -> Self {
        TreeNode { kind: NodeKind::Dir, name: name.into(), path: path.into(), children: vec![] }
    }
    fn file(name: &str, path: &str) -> Self {
        TreeNode { kind: NodeKind::File, name: name.into(), path: path.into(), children: vec![] }
    }
    pub fn is_dir(&self) -> bool {
        self.kind == NodeKind::Dir
    }
}

/// Build the tree from `(path, is_dir)` entries (decoupled from the engine's
/// `FileEntry` so this module stays pure/testable).
pub fn build_tree<'a>(files: impl IntoIterator<Item = (&'a str, bool)>) -> Vec<TreeNode> {
    let mut root = TreeNode::dir("", "");

    fn ensure_dir<'b>(root: &'b mut TreeNode, parts: &[&str]) -> &'b mut TreeNode {
        let mut node = root;
        let mut acc = String::new();
        for part in parts {
            acc = if acc.is_empty() { part.to_string() } else { format!("{acc}/{part}") };
            let idx = node
                .children
                .iter()
                .position(|c| c.is_dir() && c.name == *part);
            let idx = match idx {
                Some(i) => i,
                None => {
                    node.children.push(TreeNode::dir(part, &acc));
                    node.children.len() - 1
                }
            };
            node = &mut node.children[idx];
        }
        node
    }

    for (path, is_dir) in files {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if parts.is_empty() {
            continue;
        }
        if is_dir {
            ensure_dir(&mut root, &parts);
            continue;
        }
        let name = parts[parts.len() - 1];
        let parent = ensure_dir(&mut root, &parts[..parts.len() - 1]);
        if !parent
            .children
            .iter()
            .any(|c| c.kind == NodeKind::File && c.name == name)
        {
            parent.children.push(TreeNode::file(name, path));
        }
    }

    sort_rec(&mut root);
    root.children
}

fn sort_rec(n: &mut TreeNode) {
    n.children.sort_by(compare_nodes);
    for c in &mut n.children {
        sort_rec(c);
    }
}

/// Stem = name without its extension (only when the dot is not the first char),
/// mirroring `lastIndexOf('.') > 0 ? slice : whole`.
fn stem_of(s: &str) -> &str {
    match s.rfind('.') {
        Some(i) if i > 0 => &s[..i],
        _ => s,
    }
}

/// ALL-CAPS note stems (with at least one ASCII letter) float to the very top.
fn cap_rank(n: &TreeNode) -> i32 {
    let st = stem_of(&n.name);
    let has_letter = st.chars().any(|c| c.is_ascii_alphabetic());
    if n.kind == NodeKind::File && has_letter && st == st.to_uppercase() {
        0
    } else {
        1
    }
}

fn dir_rank(n: &TreeNode) -> i32 {
    if n.is_dir() {
        0
    } else {
        1
    }
}

/// The design's row order: ALL-CAPS notes, then folders, then files; natural
/// (numeric, case-insensitive) within each group.
pub fn compare_nodes(a: &TreeNode, b: &TreeNode) -> Ordering {
    (cap_rank(a).cmp(&cap_rank(b)))
        .then_with(|| dir_rank(a).cmp(&dir_rank(b)))
        .then_with(|| natural_cmp(&a.name, &b.name))
}

/// Approximates JS `localeCompare(_, {numeric:true, sensitivity:'base'})`:
/// case-insensitive, with runs of digits compared by numeric value.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let a: Vec<char> = a.chars().flat_map(|c| c.to_lowercase()).collect();
    let b: Vec<char> = b.chars().flat_map(|c| c.to_lowercase()).collect();
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let si = i;
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            let sj = j;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let na: String = a[si..i].iter().collect();
            let nb: String = b[sj..j].iter().collect();
            match cmp_numeric(&na, &nb) {
                Ordering::Equal => {}
                ord => return ord,
            }
        } else {
            match a[i].cmp(&b[j]) {
                Ordering::Equal => {}
                ord => return ord,
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

/// Compare two all-digit strings by numeric value without overflow.
fn cmp_numeric(a: &str, b: &str) -> Ordering {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

#[derive(Debug, Clone)]
pub struct FlatRow {
    pub node: TreeNode,
    pub depth: usize,
}

/// Flatten the tree to indented rows; children of collapsed dirs are hidden.
pub fn flatten(tree: &[TreeNode], expanded: &HashMap<String, bool>) -> Vec<FlatRow> {
    fn go(tree: &[TreeNode], expanded: &HashMap<String, bool>, depth: usize, out: &mut Vec<FlatRow>) {
        for node in tree {
            out.push(FlatRow { node: node.clone(), depth });
            if node.is_dir() && expanded.get(&node.path).copied().unwrap_or(false) {
                go(&node.children, expanded, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    go(tree, expanded, 0, &mut out);
    out
}

/// Every directory path in the tree (for "expand all" when opening a vault).
pub fn all_dir_paths(tree: &[TreeNode]) -> Vec<String> {
    fn walk(nodes: &[TreeNode], out: &mut Vec<String>) {
        for n in nodes {
            if n.is_dir() {
                out.push(n.path.clone());
                walk(&n.children, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
}

/// First README (any depth) else the first file — the default selection.
pub fn first_selectable(tree: &[TreeNode]) -> Option<String> {
    fn walk(nodes: &[TreeNode], first_file: &mut Option<String>, readme: &mut Option<String>) {
        for n in nodes {
            if n.kind == NodeKind::File {
                if first_file.is_none() {
                    *first_file = Some(n.path.clone());
                }
                if readme.is_none() && n.name.to_lowercase().contains("readme") {
                    *readme = Some(n.path.clone());
                }
            } else {
                walk(&n.children, first_file, readme);
            }
        }
    }
    let mut first_file = None;
    let mut readme = None;
    walk(tree, &mut first_file, &mut readme);
    readme.or(first_file)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(tree: &[TreeNode]) -> Vec<&str> {
        tree.iter().map(|n| n.name.as_str()).collect()
    }
    fn exp(keys: &[&str]) -> HashMap<String, bool> {
        keys.iter().map(|k| (k.to_string(), true)).collect()
    }

    #[test]
    fn nests_files_caps_float_then_folders_then_files() {
        let tree = build_tree([("README.md", false), ("notes/b.md", false), ("notes/a.md", false), ("z.md", false)]);
        assert_eq!(names(&tree), ["README.md", "notes", "z.md"]);
        let notes = tree.iter().find(|n| n.name == "notes").unwrap();
        assert!(notes.is_dir());
        assert_eq!(names(&notes.children), ["a.md", "b.md"]);
    }

    #[test]
    fn pins_full_group_order() {
        let tree = build_tree([
            ("notes.md", false), ("zebra", true), ("TODO.md", false), ("apple", true), ("README.md", false),
        ]);
        assert_eq!(names(&tree), ["README.md", "TODO.md", "apple", "zebra", "notes.md"]);
        let flat = flatten(&tree, &HashMap::new());
        assert_eq!(
            flat.iter().map(|r| r.node.name.as_str()).collect::<Vec<_>>(),
            ["README.md", "TODO.md", "apple", "zebra", "notes.md"]
        );
    }

    #[test]
    fn folders_before_files_even_when_name_sorts_first() {
        let tree = build_tree([("apple.md", false), ("zebra/x.md", false)]);
        assert_eq!(names(&tree), ["zebra", "apple.md"]);
    }

    #[test]
    fn caps_notes_above_folders() {
        let tree = build_tree([("LICENSE", false), ("src/main.ts", false), ("readme-lower.md", false)]);
        assert_eq!(names(&tree), ["LICENSE", "src", "readme-lower.md"]);
    }

    #[test]
    fn orders_numerically() {
        let tree = build_tree([("note-10.md", false), ("note-2.md", false), ("note-1.md", false)]);
        assert_eq!(names(&tree), ["note-1.md", "note-2.md", "note-10.md"]);
    }

    #[test]
    fn includes_explicit_empty_dirs() {
        let tree = build_tree([("empty", true), ("a.md", false)]);
        assert!(tree.iter().any(|n| n.name == "empty" && n.is_dir()));
    }

    #[test]
    fn skips_empty_and_ranks_dotfiles_numeric_stems() {
        let tree = build_tree([("", false), (".gitignore", false), ("Makefile", false), ("123.md", false), ("READ.md", false)]);
        assert_eq!(names(&tree), ["READ.md", ".gitignore", "123.md", "Makefile"]);
    }

    #[test]
    fn flatten_honors_expanded() {
        let tree = build_tree([("d/c.md", false), ("a.md", false)]);
        let collapsed = flatten(&tree, &HashMap::new());
        assert_eq!(collapsed.iter().map(|r| r.node.name.as_str()).collect::<Vec<_>>(), ["d", "a.md"]);
        let open = flatten(&tree, &exp(&["d"]));
        assert_eq!(open.iter().map(|r| r.node.name.as_str()).collect::<Vec<_>>(), ["d", "c.md", "a.md"]);
        assert_eq!(open.iter().find(|r| r.node.name == "c.md").unwrap().depth, 1);
    }

    #[test]
    fn all_dir_paths_lists_every_dir() {
        let tree = build_tree([("a/b/c.md", false)]);
        let mut got = all_dir_paths(&tree);
        got.sort();
        assert_eq!(got, ["a", "a/b"]);
    }

    #[test]
    fn first_selectable_prefers_readme() {
        assert_eq!(
            first_selectable(&build_tree([("x.md", false), ("docs/README.md", false)])),
            Some("docs/README.md".to_string())
        );
        assert_eq!(
            first_selectable(&build_tree([("x.md", false), ("y.md", false)])),
            Some("x.md".to_string())
        );
        assert_eq!(first_selectable(&build_tree([("only", true)])), None);
    }
}
