//! Self-tests for the hermetic git-fixture harness (`asp_e2e::gitfix`, spec §10).
//!
//! Proves the harness the git-bridge importer/round-trip tests will build on:
//! (i) every canned fixture builds with a deterministic `git log --graph`
//! topology; (ii) a real `git clone` round-trips through the smart-HTTP server;
//! (iii) protocol v2 `ls-remote` works; (iv) the token server rejects anonymous
//! clones and accepts authenticated ones; (v) `git push` through the server works;
//! plus the force-rewrite helper. Skips gracefully (with a message) if system git
//! is absent or older than 2.30.

use std::path::Path;
use std::process::{Command, Output};

use asp_e2e::gitfix::{
    criss_cross, force_rewrite_tip, foxtrot, gitignore_nested, linear_basic, merge_into_side,
    merged_prs, mid_history_root, modes_and_symlinks, octopus, pointers, renames_across_merge,
    FixtureFn, GitHttpServer,
};

// ── Expected `git log --graph --pretty=format:%s HEAD` topologies ──────────
// Deterministic because author/committer identity + clock are fixed. Trailing
// whitespace git emits on graph lines is normalized away (see `norm`).

const LINEAR_BASIC: &str = r#"*
* rename a -> a2
* add dir/c, delete dir/b
* edit a
* add a and dir/b"#;

const MERGED_PRS: &str = r#"*   Merge pull request #2 from owner/feature-2
|\
| * feature 2 work
|/
*   Merge pull request #1 from owner/feature-1
|\
| * feature 1 work
|/
* initial commit"#;

const CRISS_CROSS: &str = r#"*   Merge branch 'branch-b'
|\
| *   M2: merge branch-a into branch-b
| |\
* | \   Merge branch 'branch-a'
|\ \ \
| * | | M1: merge branch-b into branch-a
| |\| |
| | |/
| |/|
| | * B on branch-b
| |/
|/|
| * A on branch-a
|/
* C0 base"#;

const OCTOPUS: &str = r#"*---.   Octopus merge of oct-1, oct-2, oct-3
|\ \ \
| | | * oct-3 work
| | * | oct-2 work
| | |/
| * / oct-1 work
| |/
* / main advances
|/
* C0 base"#;

const MERGE_INTO_SIDE: &str = r#"*   Merge branch 'side'
|\
| *   Merge branch 'main' into side
| |\
| |/
|/|
* | main work
| * side work
|/
* C0 base"#;

const RENAMES_ACROSS_MERGE: &str = r#"*   Merge branch 'rename-side'
|\
| * rename foo -> bar on side
* | edit foo on main
|/
* add foo"#;

const MID_HISTORY_ROOT: &str = r#"*   Merge unrelated history 'graft'
|\
| * independent root
* main c1
* main c0"#;

const MODES_AND_SYMLINKS: &str = r#"* retarget symlink -> targetB
* add symlink -> targetA
* add executable script"#;

const GITIGNORE_NESTED: &str = r#"* nested gitignore with negation
* root gitignore + tracked files"#;

const POINTERS: &str = r#"* LFS pointer + gitmodules + gitlink"#;

const FOXTROT: &str = r#"*   Merge branch 'main' into feature
|\
| * C2 main work
* | F1 feature work
|/
* C1 base"#;

/// Right-trim each line so git's graph trailing whitespace doesn't matter.
fn norm(s: &str) -> String {
    s.lines().map(|l| l.trim_end()).collect::<Vec<_>>().join("\n")
}

/// Skip the whole file (with a printed reason) unless `git >= 2.30` is present.
fn git_available() -> bool {
    let out = match Command::new("git").arg("version").output() {
        Ok(o) if o.status.success() => o,
        _ => {
            eprintln!("SKIP: system `git` not found; git harness tests require git >= 2.30");
            return false;
        }
    };
    let text = String::from_utf8_lossy(&out.stdout);
    // "git version 2.51.0"
    let ver = text.split_whitespace().nth(2).unwrap_or("");
    let mut parts = ver.split('.');
    let major: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    let minor: u32 = parts.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    if (major, minor) < (2, 30) {
        eprintln!("SKIP: git {ver} < 2.30; git harness tests require git >= 2.30");
        return false;
    }
    true
}

/// A git client invocation with a hermetic, prompt-free environment (used for the
/// clone/push/ls-remote round-trips against the server).
fn client_git(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(cwd)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_ASKPASS", "true")
        .args(args)
        .output()
        .expect("spawn git")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn fixtures_build_with_expected_topology() {
    if !git_available() {
        return;
    }
    let cases: &[(&str, FixtureFn, &str)] = &[
        ("linear_basic", linear_basic, LINEAR_BASIC),
        ("merged_prs", merged_prs, MERGED_PRS),
        ("criss_cross", criss_cross, CRISS_CROSS),
        ("octopus", octopus, OCTOPUS),
        ("merge_into_side", merge_into_side, MERGE_INTO_SIDE),
        ("renames_across_merge", renames_across_merge, RENAMES_ACROSS_MERGE),
        ("mid_history_root", mid_history_root, MID_HISTORY_ROOT),
        ("modes_and_symlinks", modes_and_symlinks, MODES_AND_SYMLINKS),
        ("gitignore_nested", gitignore_nested, GITIGNORE_NESTED),
        ("pointers", pointers, POINTERS),
        ("foxtrot", foxtrot, FOXTROT),
    ];
    for (name, build, expected) in cases {
        let repo = build();
        assert!(repo.bare.exists(), "{name}: bare mirror missing");
        let got = norm(&repo.graph());
        assert_eq!(got, norm(expected), "{name}: topology mismatch\n--- got ---\n{got}\n--- want ---\n{}", norm(expected));
    }
}

#[test]
fn merge_parent_counts_are_correct() {
    if !git_available() {
        return;
    }
    // Octopus merge has 4 parents (main + 3 side branches).
    let oct = octopus();
    assert_eq!(oct.parents("HEAD").len(), 4, "octopus merge must have 4 parents");

    // Criss-cross: M1 and M2 each share the same two merge bases (A, B).
    let cc = criss_cross();
    let m1 = cc.rev("branch-a");
    let m2 = cc.rev("branch-b");
    assert_eq!(cc.parents(&m1).len(), 2);
    assert_eq!(cc.parents(&m2).len(), 2);
    // The two merge-base commits are identical for M1 and M2 → a real criss-cross.
    let bases = cc.git_ok(&["merge-base", "--all", &m1, &m2]);
    assert_eq!(bases.lines().count(), 2, "criss-cross must have two merge bases");

    // Foxtrot: main's tip first parent is the feature commit, not the mainline C2.
    let fx = foxtrot();
    let first_parent = fx.rev("HEAD^1");
    let feature_subject = fx.git_ok(&["show", "-s", "--pretty=format:%s", &first_parent]);
    assert_eq!(feature_subject, "F1 feature work", "foxtrot: first-parent must divert onto feature");
}

#[test]
fn clone_roundtrips_through_server() {
    if !git_available() {
        return;
    }
    let repo = linear_basic();
    let head = repo.head();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url("linear_basic");

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let dest = tmp.path().join("clone");

    let out = client_git(
        &home,
        tmp.path(),
        &["-c", "protocol.version=2", "clone", &url, dest.to_str().unwrap()],
    );
    assert!(out.status.success(), "clone failed: {}", String::from_utf8_lossy(&out.stderr));

    // Files + HEAD + full log round-trip.
    assert_eq!(stdout(&client_git(&home, &dest, &["rev-parse", "HEAD"])), head);
    assert!(dest.join("a2.txt").exists(), "renamed file must be present in clone");
    assert!(!dest.join("a.txt").exists(), "pre-rename path must be gone");
    let cloned_log = stdout(&client_git(&home, &dest, &["log", "--pretty=format:%s"]));
    assert_eq!(cloned_log, norm(&repo.git_ok(&["log", "--pretty=format:%s", "HEAD"])));
}

#[test]
fn ls_remote_protocol_v2() {
    if !git_available() {
        return;
    }
    let repo = merged_prs();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url("merged_prs");

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    // GIT_PROTOCOL=version=2 exercises the Git-Protocol header forwarding path.
    let out = Command::new("git")
        .current_dir(tmp.path())
        .env("HOME", &home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_PROTOCOL", "version=2")
        .args(["-c", "protocol.version=2", "ls-remote", &url])
        .output()
        .expect("spawn git");
    assert!(out.status.success(), "ls-remote v2 failed: {}", String::from_utf8_lossy(&out.stderr));
    let refs = String::from_utf8_lossy(&out.stdout);
    assert!(refs.contains("refs/heads/main"), "ls-remote must advertise main:\n{refs}");
    assert!(refs.contains("HEAD"), "ls-remote must advertise HEAD");
}

#[test]
fn token_server_requires_auth() {
    if !git_available() {
        return;
    }
    let repo = linear_basic();
    let server = GitHttpServer::spawn_with_token(repo.repo_root(), "s3kret");
    let url = server.repo_url("linear_basic");

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    // Without a token → 401 → clone fails.
    let bad = client_git(&home, tmp.path(), &["clone", &url, tmp.path().join("no-auth").to_str().unwrap()]);
    assert!(!bad.status.success(), "clone without token must fail");

    // With the token as the password (GitHub form) → succeeds.
    let authed = url.replacen("http://", "http://x-access-token:s3kret@", 1);
    let good = client_git(
        &home,
        tmp.path(),
        &["-c", "protocol.version=2", "clone", &authed, tmp.path().join("authed").to_str().unwrap()],
    );
    assert!(good.status.success(), "clone with token must succeed: {}", String::from_utf8_lossy(&good.stderr));
}

#[test]
fn push_through_server() {
    if !git_available() {
        return;
    }
    let repo = linear_basic();
    let server = GitHttpServer::spawn(repo.repo_root());
    let url = server.repo_url("linear_basic");

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let dest = tmp.path().join("clone");

    assert!(client_git(&home, tmp.path(), &["clone", &url, dest.to_str().unwrap()]).status.success());

    std::fs::write(dest.join("pushed.txt"), "pushed via smart-http\n").unwrap();
    assert!(client_git(&home, &dest, &["add", "-A"]).status.success());
    let commit = client_git(
        &home,
        &dest,
        &["-c", "user.name=Pusher", "-c", "user.email=push@asp.test", "commit", "-m", "push me"],
    );
    assert!(commit.status.success(), "commit: {}", String::from_utf8_lossy(&commit.stderr));
    let new_head = stdout(&client_git(&home, &dest, &["rev-parse", "HEAD"]));

    let push = client_git(&home, &dest, &["push", "origin", "HEAD:main"]);
    assert!(push.status.success(), "push failed: {}", String::from_utf8_lossy(&push.stderr));

    // The bare repo's main now points at the pushed commit.
    assert_eq!(stdout(&client_git(&home, &repo.bare, &["rev-parse", "main"])), new_head);
}

#[test]
fn force_rewrite_tip_rewrites_bare() {
    if !git_available() {
        return;
    }
    let repo = criss_cross();
    let old = repo.head();
    let new_sha = force_rewrite_tip(&repo.bare);
    assert_ne!(old, new_sha, "force-rewrite must change the tip sha");

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    assert_eq!(stdout(&client_git(&home, &repo.bare, &["rev-parse", "main"])), new_sha);
    // The rewrite is not a descendant of the old tip (upstream history rewrite).
    let not_ancestor = client_git(&home, &repo.bare, &["merge-base", "--is-ancestor", &old, &new_sha]);
    assert!(!not_ancestor.status.success(), "old tip must NOT be an ancestor of the rewrite");
}
