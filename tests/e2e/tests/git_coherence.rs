//! *Derived git:* deterministic SHAs converge cross-node; an unmodified `git`
//! can `log`/`checkout` the derived repo; the repo is engine-owned (`.asp/git`,
//! no `.git` at the vault root) so it coexists with a project's own repo; the
//! read-only allowlist rejects every mutating verb.

use asp_e2e::{temp_root, Hub, Node};
use std::process::Command;

const SECRET: &str = "k";

fn git(git_dir: &std::path::Path, args: &[&str]) -> (bool, String) {
    let out = Command::new("git").arg("--git-dir").arg(git_dir).args(args).output().expect("git");
    (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string())
}

#[test]
fn derived_main_sha_converges_cross_node() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    a.write("readme.md", b"# Project\nhello\n");
    a.write("src/lib.rs", b"pub fn add(a:i32,b:i32)->i32{a+b}\n");
    a.sync(&url, Some(SECRET));

    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));

    let ha = a.head();
    let hb = b.head();
    assert!(!ha.is_empty(), "A has a derived main SHA");
    assert_eq!(ha, hb, "derived main SHA converges across nodes with the same tree");
}

#[test]
fn stock_git_can_read_and_checkout_the_derived_repo() {
    let root = temp_root();
    let a = Node::new(root.path(), "A");
    a.init();
    a.write("a/b.md", b"nested\n");
    a.write("top.md", b"top\n");
    a.commit();

    let git_dir = a.dir.join(".asp/git");
    let (ok, log) = git(&git_dir, &["log", "--oneline"]);
    assert!(ok && !log.trim().is_empty(), "stock git can log the derived repo: {log}");

    // Checkout via a fresh clone — an unmodified git materializes the tree.
    let work = root.path().join("checkout");
    let out = Command::new("git")
        .args(["clone", "-q", git_dir.to_str().unwrap(), work.to_str().unwrap()])
        .output()
        .expect("git clone");
    assert!(out.status.success(), "git clone of derived repo: {}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(std::fs::read_to_string(work.join("a/b.md")).unwrap(), "nested\n");
    assert_eq!(std::fs::read_to_string(work.join("top.md")).unwrap(), "top\n");
}

#[test]
fn engine_owns_repo_no_dot_git_at_root() {
    let root = temp_root();
    let a = Node::new(root.path(), "A");
    a.init();
    a.write("x.md", b"x\n");
    a.commit();
    assert!(a.dir.join(".asp/git").exists(), "engine repo lives at .asp/git");
    assert!(!a.dir.join(".git").exists(), "no .git at the vault root — coexists with a project's repo");
}

#[test]
fn read_only_allowlist_rejects_mutating_verbs() {
    let root = temp_root();
    let a = Node::new(root.path(), "A");
    a.init();
    a.write("x.md", b"x\n");
    a.commit();

    // Allowed read verb works.
    let (ok, _, _) = a.try_run(&["git", "log", "--oneline"]);
    assert!(ok, "read-only verbs are allowed");

    // Mutating verbs are refused (deny-by-default).
    for verb in [["git", "commit"], ["git", "checkout"], ["git", "merge"], ["git", "push"]] {
        let (ok, _out, err) = a.try_run(&verb);
        assert!(!ok, "`{}` must be refused", verb[1]);
        assert!(err.contains("refused") || err.contains("read-only"), "helpful refusal for {}: {err}", verb[1]);
    }
}
