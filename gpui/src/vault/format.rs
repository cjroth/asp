//! Small pure formatting/util helpers, ported 1:1 from desktop `src/vault/format.ts`.
//! Kept pure (no clock/RNG side effects baked in) so they're deterministically testable.

/// `basename` strips trailing slashes and parent dirs. Mirrors
/// `p.split('/').filter(Boolean).pop() || p` — empty parts fall back to the input.
pub fn basename(p: &str) -> &str {
    p.split('/').filter(|s| !s.is_empty()).next_back().unwrap_or(p)
}

/// A human "time ago" for a wall-clock unix-seconds timestamp (or em-dash if none/zero).
/// `now_secs` is injected (matches the desktop tests mocking `Date.now()`).
pub fn rel_time(sec: Option<i64>, now_secs: i64) -> String {
    let sec = match sec {
        Some(s) if s != 0 => s,
        _ => return "—".to_string(),
    };
    let d = (now_secs - sec).max(0);
    if d < 5 {
        "just now".to_string()
    } else if d < 60 {
        format!("{d}s ago")
    } else if d < 3600 {
        format!("{}m ago", d / 60)
    } else if d < 86400 {
        format!("{}h ago", d / 3600)
    } else if d < 172800 {
        "yesterday".to_string()
    } else {
        format!("{}d ago", d / 86400)
    }
}

/// Abbreviate an ssh identity to a short, readable fingerprint.
/// Strips a leading `ssh-<type> ` prefix, then keeps `head…tail` if long.
pub fn short_fingerprint(identity: &str) -> String {
    // Replace(/^ssh-\S+\s+/, ''): drop a leading `ssh-<nonspace>` + whitespace run.
    let cleaned = strip_ssh_prefix(identity).trim();
    let chars: Vec<char> = cleaned.chars().collect();
    if chars.len() <= 14 {
        return cleaned.to_string();
    }
    let head: String = chars[..8].iter().collect();
    let tail: String = chars[chars.len() - 4..].iter().collect();
    format!("{head}…{tail}")
}

fn strip_ssh_prefix(s: &str) -> &str {
    let Some(rest) = s.strip_prefix("ssh-") else {
        return s;
    };
    // skip the non-space run, then the whitespace run; only strip if both present.
    let after_type = rest.trim_start_matches(|c: char| !c.is_whitespace());
    if after_type.len() == rest.len() {
        return s; // no whitespace after `ssh-<type>` → no match, leave as-is
    }
    let after_ws = after_type.trim_start_matches(char::is_whitespace);
    after_ws
}

/// The safe Crockford-ish alphabet (no ambiguous chars) for access keys.
pub const ACCESS_KEY_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// A random `XXXX-XXXX-XXXX-XXXX` access key. `rand` yields a value in `0..32`
/// per character (injected for testability; pass a real RNG at the call site).
pub fn make_access_key(mut rand: impl FnMut() -> usize) -> String {
    let grp = |rand: &mut dyn FnMut() -> usize| -> String {
        (0..4)
            .map(|_| ACCESS_KEY_ALPHABET[rand() % 32] as char)
            .collect()
    };
    let mut r = &mut rand as &mut dyn FnMut() -> usize;
    [grp(&mut r), grp(&mut r), grp(&mut r), grp(&mut r)].join("-")
}

/// A free `untitled[-n]<ext>` name given existing sibling names.
pub fn free_name(siblings: &std::collections::HashSet<String>, ext: &str) -> String {
    let mut name = format!("untitled{ext}");
    let mut i = 0u32;
    while siblings.contains(&name) {
        i += 1;
        name = format!("untitled-{i}{ext}");
    }
    name
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn basename_strips_trailing_slashes_and_dirs() {
        assert_eq!(basename("/a/b/c"), "c");
        assert_eq!(basename("/a/b/"), "b");
        assert_eq!(basename("solo"), "solo");
        assert_eq!(basename("/"), "/"); // empty parts → fall back to input
        assert_eq!(basename(""), "");
    }

    #[test]
    fn rel_time_buckets() {
        let now = 1_700_000_000;
        assert_eq!(rel_time(None, now), "—");
        assert_eq!(rel_time(Some(0), now), "—");
        assert_eq!(rel_time(Some(now - 2), now), "just now");
        assert_eq!(rel_time(Some(now - 30), now), "30s ago");
        assert_eq!(rel_time(Some(now - 120), now), "2m ago");
        assert_eq!(rel_time(Some(now - 7200), now), "2h ago");
        assert_eq!(rel_time(Some(now - 100000), now), "yesterday");
        assert_eq!(rel_time(Some(now - 300000), now), "3d ago");
    }

    #[test]
    fn short_fingerprint_abbreviates_long_keys_keeps_short() {
        assert_eq!(
            short_fingerprint("ssh-ed25519 ABCDEFGHIJKLMNOPQRSTUV"),
            "ABCDEFGH…STUV"
        );
        assert_eq!(short_fingerprint("ssh-ed25519 short"), "short");
    }

    #[test]
    fn make_access_key_matches_pattern() {
        // deterministic counter RNG; just assert structure + alphabet.
        let mut n = 0usize;
        let k = make_access_key(|| {
            n += 1;
            n
        });
        let groups: Vec<&str> = k.split('-').collect();
        assert_eq!(groups.len(), 4);
        for g in groups {
            assert_eq!(g.chars().count(), 4);
            assert!(g.bytes().all(|b| ACCESS_KEY_ALPHABET.contains(&b)));
        }
    }

    #[test]
    fn free_name_finds_first_free_untitled() {
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<HashSet<_>>();
        assert_eq!(free_name(&HashSet::new(), ".md"), "untitled.md");
        assert_eq!(free_name(&s(&["untitled.md"]), ".md"), "untitled-1.md");
        assert_eq!(
            free_name(&s(&["untitled.md", "untitled-1.md"]), ".md"),
            "untitled-2.md"
        );
        assert_eq!(
            free_name(&s(&["untitled.md", "untitled-1.md", "untitled-2.md"]), ".md"),
            "untitled-3.md"
        );
        assert_eq!(free_name(&HashSet::new(), ""), "untitled");
    }
}
