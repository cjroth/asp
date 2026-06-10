//! Scope-closure property (§Testing). The ignore matcher is a *sync gate*: a
//! path it wrongly admits gets versioned and pushed to every peer. This session
//! shipped two real leaks through that gate — a nested `proj/.git/**` (the
//! matcher only checked the FIRST path segment) and `.context/id_ed25519` (the
//! node's private key). Both passed the old unit tests because those tests only
//! listed the cases the author already had in mind — they encoded the same blind
//! spot as the bug.
//!
//! This guards the *negative space* instead: generate adversarial vault trees
//! (ignored dir names at every depth, plus lookalikes that must NOT be caught)
//! and assert `Scope` partitions them by an INDEPENDENT oracle. Because the
//! oracle is a separate, trivial implementation of the spec ("ignored iff some
//! path segment names a private/editor/VCS dir"), any matcher that drifts from
//! that spec — e.g. one that checks only the top segment — fails here.

use asp_core::scope::Scope;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

/// The dirs that must ALWAYS be out of scope, at ANY depth, regardless of an
/// ignore file. Kept here as an independent restatement of the contract — NOT
/// imported from the matcher — so this test fails if the matcher's set drifts.
const ALWAYS_IGNORED: &[&str] = &[".asp", ".context", ".git", ".obsidian", ".trash"];

/// Segments that look like an ignored dir but are NOT one — these must sync.
/// (Real bugs hide here: over-eager matching that swallows `git-tips/` is just
/// as wrong as under-eager matching that leaks `.git/`.)
const LOOKALIKES: &[&str] =
    &["git", "gitland", "git-tips", ".gitignore", "context", "obsidian", "obsidian-notes", "aspire", "trashcan"];

const REAL: &[&str] = &["notes", "a.md", "b.txt", "dir", "src", "x.rs", "README.md", "proj", "deep"];

/// Independent oracle: a path is ignored iff one of its `/`-segments is exactly
/// an always-ignored dir name. (Default scope only — no extra patterns.)
fn oracle_ignored(path: &str) -> bool {
    path.split('/').any(|seg| ALWAYS_IGNORED.contains(&seg))
}

fn rand_path(r: &mut StdRng) -> String {
    let depth = r.gen_range(1..=5);
    (0..depth)
        .map(|_| match r.gen_range(0..10u8) {
            // Bias toward the interesting cases: ignored names and lookalikes,
            // at every position (not just the root).
            0..=2 => ALWAYS_IGNORED[r.gen_range(0..ALWAYS_IGNORED.len())],
            3..=5 => LOOKALIKES[r.gen_range(0..LOOKALIKES.len())],
            _ => REAL[r.gen_range(0..REAL.len())],
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[test]
fn default_scope_partitions_adversarial_trees_by_the_oracle() {
    let s = Scope::default();
    let mut r = StdRng::seed_from_u64(0x5C0FE);
    let mut saw_ignored = 0u32;
    let mut saw_synced = 0u32;
    for _ in 0..50_000 {
        let p = rand_path(&mut r);
        let want = oracle_ignored(&p);
        assert_eq!(s.ignored(&p), want, "scope disagreed with oracle on {p:?}");
        if want {
            saw_ignored += 1;
        } else {
            saw_synced += 1;
        }
    }
    // Sanity: the generator actually exercised BOTH sides of the partition (a
    // matcher that always-returns-false would otherwise pass a one-sided test).
    assert!(saw_ignored > 1000, "generator never hit ignored paths ({saw_ignored})");
    assert!(saw_synced > 1000, "generator never hit synced paths ({saw_synced})");
}

#[test]
fn nested_ignored_dirs_at_every_depth() {
    // The exact shape that leaked: a cloned repo kept as reference material, and
    // the legacy private-home key, both buried below the root.
    let s = Scope::default();
    for p in [
        "context/gridland/.git/objects/pack/pack-abc.pack",
        "a/b/c/.git/config",
        ".context/id_ed25519",
        "notes/proj/.obsidian/workspace.json",
        "x/y/.trash/old.md",
    ] {
        assert!(s.ignored(p), "must ignore {p:?}");
    }
    for p in ["notes/git-tips/howto.md", "projects/gitland/readme.md", "docs/.gitignore", "context/note.md"] {
        assert!(!s.ignored(p), "must NOT ignore {p:?}");
    }
}

#[test]
fn ignore_file_cannot_re_include_a_hard_ignored_dir() {
    // A hostile or careless `.aspignore` must not be able to un-ignore the
    // private/VCS dirs at any depth — the guarantee is non-overridable.
    let s = Scope::parse("!.git\n!.context\n!.obsidian\n*.md\n");
    for p in ["proj/.git/HEAD", ".context/id_ed25519", "deep/x/.obsidian/app.json"] {
        assert!(s.ignored(p), "negation must not re-include {p:?}");
    }
    // ...while ordinary patterns still apply to in-scope paths.
    assert!(s.ignored("notes/plan.md"));
    assert!(!s.ignored("notes/plan.txt"));
}
