//! Scope / ignore matching (§Surfaces, `asp scope`). A hand-rolled gitignore-
//! style matcher (the core carries no `regex` dep where a small differentially-
//! tested equivalent will do, §Implementation). Always ignores the engine's own
//! `.asp/` directory so the store never tries to version itself.

/// The engine's private directory, never versioned.
pub const PRIVATE_DIR: &str = ".asp";

/// Directory names that are ALWAYS out of scope at ANY depth, regardless of
/// `.aspignore`. They hold node-private material or editor/VCS internals that
/// must never be versioned or synced:
///   `.asp`/`.context` — the engine's private home (current + legacy name);
///     `.context` holds the node's id_ed25519, so without this guard that
///     PRIVATE KEY would sync to every peer.
///   `.git`           — version-control internals. Matched at any depth because
///     vaults commonly hold cloned repos as reference material; a nested
///     `proj/.git/objects/pack/*.pack` is multi-MB and explodes the synced log.
///   `.obsidian`      — the editor's config + a plugin's own state/binary.
///   `.trash`         — local trash.
const ALWAYS_IGNORE_DIRS: &[&str] = &[PRIVATE_DIR, ".context", ".git", ".obsidian", ".trash"];

/// A compiled set of ignore patterns.
#[derive(Clone, Default)]
pub struct Scope {
    patterns: Vec<Pattern>,
}

#[derive(Clone)]
struct Pattern {
    /// Glob with `*` (any run within a segment) and `**` (any number of segments).
    glob: String,
    /// Anchored to the root (leading `/`).
    anchored: bool,
    /// Directory-only (trailing `/`).
    dir_only: bool,
    /// Negation (`!`).
    negate: bool,
}

impl Scope {
    /// Parse `.aspignore` content. Blank lines and `#` comments are skipped.
    pub fn parse(content: &str) -> Scope {
        let mut patterns = Vec::new();
        for raw in content.lines() {
            let line = raw.trim_end();
            if line.trim().is_empty() || line.trim_start().starts_with('#') {
                continue;
            }
            let mut s = line;
            let negate = s.starts_with('!');
            if negate {
                s = &s[1..];
            }
            let anchored = s.starts_with('/');
            if anchored {
                s = &s[1..];
            }
            let dir_only = s.ends_with('/');
            if dir_only {
                s = &s[..s.len() - 1];
            }
            patterns.push(Pattern { glob: s.to_string(), anchored, dir_only, negate });
        }
        Scope { patterns }
    }

    /// Should `rel_path` (a forward-slash relative path) be ignored?
    pub fn ignored(&self, rel_path: &str) -> bool {
        // Always-ignored dirs (private/editor/VCS) at ANY depth are out of scope.
        if rel_path.split('/').any(|seg| ALWAYS_IGNORE_DIRS.contains(&seg)) {
            return true;
        }
        let mut ignored = false;
        for p in &self.patterns {
            if p.matches(rel_path) {
                ignored = !p.negate;
            }
        }
        ignored
    }
}

impl Pattern {
    fn matches(&self, path: &str) -> bool {
        if self.anchored {
            self.match_from(path)
        } else {
            // Unanchored: match against the full path or any suffix segment start.
            if self.match_from(path) {
                return true;
            }
            let mut rest = path;
            while let Some(i) = rest.find('/') {
                rest = &rest[i + 1..];
                if self.match_from(rest) {
                    return true;
                }
            }
            false
        }
    }

    /// Match the glob against `path` from its start. `dir_only` matches a prefix
    /// directory (so `build/` ignores `build/x`).
    fn match_from(&self, path: &str) -> bool {
        if self.dir_only {
            // Match the glob against the first segment(s), allowing children.
            if glob_match(&self.glob, path) {
                return true;
            }
            // `glob/` should match `glob/anything`.
            if let Some(stripped) = path.strip_prefix(&format!("{}/", self.glob)) {
                let _ = stripped;
                return true;
            }
            // Also match when glob matches a leading prefix ending at a '/'.
            for (i, c) in path.char_indices() {
                if c == '/' && glob_match(&self.glob, &path[..i]) {
                    return true;
                }
            }
            false
        } else {
            glob_match(&self.glob, path) || {
                // A plain file pattern also ignores everything under a matching dir.
                for (i, c) in path.char_indices() {
                    if c == '/' && glob_match(&self.glob, &path[..i]) {
                        return true;
                    }
                }
                false
            }
        }
    }
}

/// Glob with `*` (any chars except `/`) and `**` (any chars incl `/`).
fn glob_match(pat: &str, text: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let t: Vec<char> = text.chars().collect();
    // Memoize states already proven non-matching. Without this, a pattern with
    // several `*` against a long near-matching text (e.g. `*a*a*a*…*b` vs
    // `aaaa…`) backtracks exponentially — a CPU DoS reachable through a hostile
    // `.aspignore` synced from a peer. Memoizing failed (pi, ti) states bounds the
    // matcher to O(|pat| · |text|) while preserving exact semantics.
    let mut failed: std::collections::HashSet<(usize, usize)> = std::collections::HashSet::new();
    gm(&p, 0, &t, 0, &mut failed)
}

fn gm(p: &[char], pi: usize, t: &[char], ti: usize, failed: &mut std::collections::HashSet<(usize, usize)>) -> bool {
    if pi == p.len() {
        return ti == t.len();
    }
    if failed.contains(&(pi, ti)) {
        return false;
    }
    let matched = gm_inner(p, pi, t, ti, failed);
    if !matched {
        failed.insert((pi, ti));
    }
    matched
}

fn gm_inner(p: &[char], pi: usize, t: &[char], ti: usize, failed: &mut std::collections::HashSet<(usize, usize)>) -> bool {
    if p[pi] == '*' {
        // `**` — any chars including '/'.
        if pi + 1 < p.len() && p[pi + 1] == '*' {
            let mut npi = pi + 2;
            if npi < p.len() && p[npi] == '/' {
                npi += 1; // `**/` absorbs the slash optionally
            }
            // try consuming 0..=len chars
            for k in ti..=t.len() {
                if gm(p, npi, t, k, failed) {
                    return true;
                }
            }
            return gm(p, npi, t, ti, failed);
        }
        // single `*` — any chars except '/'.
        let mut k = ti;
        loop {
            if gm(p, pi + 1, t, k, failed) {
                return true;
            }
            if k == t.len() || t[k] == '/' {
                return false;
            }
            k += 1;
        }
    }
    if p[pi] == '?' {
        if ti < t.len() && t[ti] != '/' {
            return gm(p, pi + 1, t, ti + 1, failed);
        }
        return false;
    }
    if ti < t.len() && p[pi] == t[ti] {
        return gm(p, pi + 1, t, ti + 1, failed);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_dir_always_ignored() {
        let s = Scope::default();
        assert!(s.ignored(".asp"));
        assert!(s.ignored(".asp/asp.db"));
        assert!(!s.ignored("notes/a.md"));
    }

    #[test]
    fn legacy_private_dir_and_key_always_ignored() {
        // A vault from before the rename: the private key must never sync, even
        // with an empty (or hostile) ignore file.
        let s = Scope::parse("!.context\n!.context/id_ed25519\n");
        assert!(s.ignored(".context"));
        assert!(s.ignored(".context/id_ed25519"));
        assert!(s.ignored(".context/state"));
        assert!(!s.ignored("context/note.md")); // a real "context" note dir is fine
    }

    #[test]
    fn nested_vcs_editor_dirs_ignored_at_any_depth() {
        let s = Scope::default();
        // A cloned repo kept as reference material: its multi-MB pack must not sync.
        assert!(s.ignored("context/gridland/.git/objects/pack/pack-abc.pack"));
        assert!(s.ignored("notes/proj/.obsidian/workspace.json"));
        assert!(s.ignored("a/b/.trash/x.md"));
        // But a real note isn't caught just because a parent name resembles one.
        assert!(!s.ignored("notes/git-tips/howto.md"));
        assert!(!s.ignored("projects/gitland/readme.md"));
        assert!(!s.ignored(".gitignore")); // a file, not the .git dir
    }

    #[test]
    fn basic_patterns() {
        let s = Scope::parse("*.log\nbuild/\n/secret.txt\n");
        assert!(s.ignored("a.log"));
        assert!(s.ignored("deep/b.log"));
        assert!(s.ignored("build/out"));
        assert!(s.ignored("nested/build/out"));
        assert!(s.ignored("secret.txt"));
        assert!(!s.ignored("deep/secret.txt")); // anchored
        assert!(!s.ignored("a.md"));
    }

    #[test]
    fn negation() {
        let s = Scope::parse("*.log\n!keep.log\n");
        assert!(s.ignored("x.log"));
        assert!(!s.ignored("keep.log"));
    }

    #[test]
    fn double_star() {
        let s = Scope::parse("docs/**/tmp\n");
        assert!(s.ignored("docs/a/b/tmp"));
        assert!(s.ignored("docs/tmp"));
    }

    #[test]
    fn pathological_glob_does_not_backtrack_exponentially() {
        // A hostile `.aspignore` (synced from a peer) with many `*` against a long
        // near-matching path used to blow up to O(2^n). The matcher is memoized to
        // O(|pat|·|text|); this many-`*` pattern over a long non-matching text must
        // resolve effectively instantly. Completion *is* the assertion — without the
        // memo this test would hang. We also pin the (correct) negative result.
        let pat = "*a".repeat(24); // 24 alternations of `*a`
        let text = "a".repeat(200); // matches the a's but the pattern needs more → false
        let s = Scope::parse(&format!("{pat}b\n"));
        let start = std::time::Instant::now();
        let hit = s.ignored(&text);
        assert!(!hit, "pattern requires a trailing 'b' the text lacks");
        assert!(start.elapsed().as_secs() < 2, "glob matcher backtracked (took {:?})", start.elapsed());
    }

    #[test]
    fn memoized_matcher_preserves_semantics() {
        // The memo only caches *failures*; matching behaviour is unchanged.
        let s = Scope::parse("src/**/*.rs\n");
        assert!(s.ignored("src/a/b/c.rs"));
        assert!(s.ignored("src/x.rs"));
        assert!(!s.ignored("src/x.md"));
        let s2 = Scope::parse("a*b*c\n");
        assert!(s2.ignored("axxbyyc"));
        assert!(!s2.ignored("axxbyy"));
    }
}
