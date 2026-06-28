//! Open-tabs + active-file URL-hash model, ported 1:1 from desktop
//! `src/vault/tabs.ts`. Pure transforms (persistence lives in the app layer).
//!
//! Hash scheme: `#<encodeURIComponent(vaultId)>/<encodeURIComponent(path)>` —
//! both halves fully percent-encoded so the single literal `/` is unambiguous.

/// `encodeURIComponent` — percent-encode every byte except the JS unreserved set
/// `A-Za-z0-9 - _ . ! ~ * ' ( )`.
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        let keep = b.is_ascii_alphanumeric()
            || matches!(b, b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')');
        if keep {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(hex_upper(b >> 4));
            out.push(hex_upper(b & 0xf));
        }
    }
    out
}

fn hex_upper(n: u8) -> char {
    char::from_digit(n as u32, 16).unwrap().to_ascii_uppercase()
}

/// `decodeURIComponent` — decode `%XX` byte sequences as UTF-8. Returns `None`
/// on malformed escapes or invalid UTF-8 (mirrors JS throwing).
pub fn decode_uri_component(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                if i + 2 >= bytes.len() {
                    return None;
                }
                let hi = (bytes[i + 1] as char).to_digit(16)?;
                let lo = (bytes[i + 2] as char).to_digit(16)?;
                out.push((hi * 16 + lo) as u8);
                i += 3;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

pub fn build_hash(vault_id: &str, path: &str) -> String {
    format!(
        "#{}/{}",
        encode_uri_component(vault_id),
        encode_uri_component(path)
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HashTarget {
    pub vault_id: String,
    pub path: String,
}

pub fn parse_hash(hash: &str) -> Option<HashTarget> {
    if hash.is_empty() {
        return None;
    }
    let body = hash.strip_prefix('#').unwrap_or(hash);
    if body.is_empty() {
        return None;
    }
    let slash = body.find('/')?;
    // need non-empty both sides
    if slash == 0 || slash == body.len() - 1 {
        return None;
    }
    let vault_id = decode_uri_component(&body[..slash])?;
    let path = decode_uri_component(&body[slash + 1..])?;
    Some(HashTarget { vault_id, path })
}

/// Append `path` if not already open.
pub fn with_tab(tabs: &[String], path: &str) -> Vec<String> {
    let mut out = tabs.to_vec();
    if !tabs.iter().any(|t| t == path) {
        out.push(path.to_string());
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloseResult {
    pub tabs: Vec<String>,
    pub active: Option<String>,
}

/// Close `path`. When it's active, prefer the next tab, else the previous, else None.
pub fn close_tab(tabs: &[String], active: Option<&str>, path: &str) -> CloseResult {
    let idx = match tabs.iter().position(|t| t == path) {
        Some(i) => i,
        None => {
            return CloseResult {
                tabs: tabs.to_vec(),
                active: active.map(String::from),
            }
        }
    };
    let next: Vec<String> = tabs.iter().filter(|t| t.as_str() != path).cloned().collect();
    if Some(path) != active {
        return CloseResult { tabs: next, active: active.map(String::from) };
    }
    if next.is_empty() {
        return CloseResult { tabs: next, active: None };
    }
    let new_active = if idx < next.len() {
        next[idx].clone()
    } else {
        next[next.len() - 1].clone()
    };
    CloseResult { tabs: next, active: Some(new_active) }
}

/// Remap a renamed/moved file and any tab under a renamed FOLDER subtree.
pub fn remap_tabs(tabs: &[String], old_path: &str, new_path: &str) -> Vec<String> {
    let prefix = format!("{old_path}/");
    let mut out: Vec<String> = Vec::new();
    for t in tabs {
        let mapped = if t == old_path {
            new_path.to_string()
        } else if let Some(rest) = t.strip_prefix(&prefix) {
            format!("{new_path}/{rest}")
        } else {
            t.clone()
        };
        if !out.contains(&mapped) {
            out.push(mapped);
        }
    }
    out
}

/// Drop tabs matching `paths` exactly or under one as a folder subtree.
pub fn remove_tabs(tabs: &[String], paths: &[String]) -> Vec<String> {
    tabs.iter()
        .filter(|t| {
            !paths.iter().any(|p| *t == p)
                && !paths.iter().any(|p| t.starts_with(&format!("{p}/")))
        })
        .cloned()
        .collect()
}

/// Move the tab at `from` to `to`. Out-of-range / no-op returns the original.
pub fn reorder_tabs(tabs: &[String], from: usize, to: usize) -> Vec<String> {
    if from == to || from >= tabs.len() || to >= tabs.len() {
        return tabs.to_vec();
    }
    let mut next = tabs.to_vec();
    let moved = next.remove(from);
    next.insert(to, moved);
    next
}

pub fn close_others(tabs: &[String], path: &str) -> Vec<String> {
    tabs.iter().filter(|t| t.as_str() == path).cloned().collect()
}

pub fn close_to_left(tabs: &[String], path: &str) -> Vec<String> {
    match tabs.iter().position(|t| t == path) {
        Some(i) => tabs[i..].to_vec(),
        None => tabs.to_vec(),
    }
}

pub fn close_to_right(tabs: &[String], path: &str) -> Vec<String> {
    match tabs.iter().position(|t| t == path) {
        Some(i) => tabs[..=i].to_vec(),
        None => tabs.to_vec(),
    }
}

pub fn close_all() -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn hash_round_trips_plain() {
        let h = build_hash("vid1", "README.md");
        assert_eq!(h, "#vid1/README.md");
        assert_eq!(
            parse_hash(&h),
            Some(HashTarget { vault_id: "vid1".into(), path: "README.md".into() })
        );
    }

    #[test]
    fn hash_encodes_slashes() {
        let h = build_hash("vid1", "notes/sub/a.md");
        assert_eq!(h, "#vid1/notes%2Fsub%2Fa.md");
        assert_eq!(parse_hash(&h).unwrap().path, "notes/sub/a.md");
    }

    #[test]
    fn hash_round_trips_spaces_unicode() {
        let path = "my folder/héllo wörld 📓.md";
        let vault_id = "vault id with spaces";
        let h = build_hash(vault_id, path);
        let t = parse_hash(&h).unwrap();
        assert_eq!(t.vault_id, vault_id);
        assert_eq!(t.path, path);
    }

    #[test]
    fn parse_hash_without_leading_hash() {
        assert_eq!(parse_hash("vid1/a.md").unwrap().path, "a.md");
    }

    #[test]
    fn parse_hash_null_cases() {
        assert_eq!(parse_hash(""), None);
        assert_eq!(parse_hash("#"), None);
        assert_eq!(parse_hash("#nopathhere"), None);
        assert_eq!(parse_hash("#/onlypath"), None);
        assert_eq!(parse_hash("#vaultonly/"), None);
    }

    #[test]
    fn parse_hash_malformed_percent() {
        assert_eq!(parse_hash("#%E0%A4%A/x"), None);
        assert_eq!(parse_hash("#ok/%"), None);
    }

    #[test]
    fn with_tab_behaviors() {
        assert_eq!(with_tab(&v(&["a"]), "b"), v(&["a", "b"]));
        assert_eq!(with_tab(&v(&["a", "b"]), "a"), v(&["a", "b"]));
        assert_eq!(with_tab(&v(&[]), "a"), v(&["a"]));
    }

    #[test]
    fn close_tab_neighbor_selection() {
        assert_eq!(
            close_tab(&v(&["a", "b", "c"]), Some("b"), "a"),
            CloseResult { tabs: v(&["b", "c"]), active: Some("b".into()) }
        );
        assert_eq!(
            close_tab(&v(&["a", "b", "c"]), Some("b"), "b"),
            CloseResult { tabs: v(&["a", "c"]), active: Some("c".into()) }
        );
        assert_eq!(
            close_tab(&v(&["a", "b", "c"]), Some("c"), "c"),
            CloseResult { tabs: v(&["a", "b"]), active: Some("b".into()) }
        );
        assert_eq!(
            close_tab(&v(&["a", "b", "c"]), Some("a"), "a"),
            CloseResult { tabs: v(&["b", "c"]), active: Some("b".into()) }
        );
        assert_eq!(
            close_tab(&v(&["a"]), Some("a"), "a"),
            CloseResult { tabs: v(&[]), active: None }
        );
        assert_eq!(
            close_tab(&v(&["a", "b"]), Some("a"), "zzz"),
            CloseResult { tabs: v(&["a", "b"]), active: Some("a".into()) }
        );
    }

    #[test]
    fn remap_tabs_behaviors() {
        assert_eq!(remap_tabs(&v(&["a.md", "b.md"]), "a.md", "c.md"), v(&["c.md", "b.md"]));
        assert_eq!(
            remap_tabs(&v(&["notes/a.md", "notesX.md", "notes/sub/b.md"]), "notes", "archive"),
            v(&["archive/a.md", "notesX.md", "archive/sub/b.md"])
        );
        assert_eq!(remap_tabs(&v(&["a.md", "b.md"]), "a.md", "b.md"), v(&["b.md"]));
        assert_eq!(remap_tabs(&v(&["x.md"]), "a.md", "c.md"), v(&["x.md"]));
    }

    #[test]
    fn remove_tabs_behaviors() {
        assert_eq!(remove_tabs(&v(&["a.md", "b.md", "c.md"]), &v(&["b.md"])), v(&["a.md", "c.md"]));
        assert_eq!(
            remove_tabs(&v(&["notes/a.md", "notes/sub/b.md", "other.md"]), &v(&["notes"])),
            v(&["other.md"])
        );
        assert_eq!(remove_tabs(&v(&["notes.md", "notes/a.md"]), &v(&["notes"])), v(&["notes.md"]));
        assert_eq!(remove_tabs(&v(&["a", "b", "c"]), &v(&["a", "c"])), v(&["b"]));
    }

    #[test]
    fn reorder_tabs_behaviors() {
        assert_eq!(reorder_tabs(&v(&["a", "b", "c", "d"]), 0, 2), v(&["b", "c", "a", "d"]));
        assert_eq!(reorder_tabs(&v(&["a", "b", "c", "d"]), 3, 1), v(&["a", "d", "b", "c"]));
        assert_eq!(reorder_tabs(&v(&["a", "b", "c"]), 1, 1), v(&["a", "b", "c"]));
        assert_eq!(reorder_tabs(&v(&["a", "b"]), 5, 0), v(&["a", "b"]));
        assert_eq!(reorder_tabs(&v(&["a", "b"]), 0, 9), v(&["a", "b"]));
    }

    #[test]
    fn close_others_left_right_all() {
        assert_eq!(close_others(&v(&["a", "b", "c"]), "b"), v(&["b"]));
        assert_eq!(close_others(&v(&["a", "b"]), "zzz"), v(&[]));
        assert_eq!(close_to_left(&v(&["a", "b", "c"]), "b"), v(&["b", "c"]));
        assert_eq!(close_to_left(&v(&["a", "b", "c"]), "a"), v(&["a", "b", "c"]));
        assert_eq!(close_to_left(&v(&["a", "b"]), "zzz"), v(&["a", "b"]));
        assert_eq!(close_to_right(&v(&["a", "b", "c"]), "b"), v(&["a", "b"]));
        assert_eq!(close_to_right(&v(&["a", "b", "c"]), "c"), v(&["a", "b", "c"]));
        assert_eq!(close_to_right(&v(&["a", "b"]), "zzz"), v(&["a", "b"]));
        assert_eq!(close_all(), v(&[]));
    }
}
