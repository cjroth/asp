//! CLI branches (§2, §7): the `asp branch` surface + cross-node branch sync.
//! The local lifecycle test is deterministic (no network); the propagation test
//! drives two real `asp` processes through a hub.

use asp_e2e::{temp_root, wait_until, Hub, Node};
use std::time::Duration;

const SECRET: &str = "branch-secret";

#[test]
fn cli_branch_lifecycle_local() {
    // create / list / checkout / delete + on-disk isolation, all through the CLI.
    let root = temp_root();
    let a = Node::new(root.path(), "A");
    a.init();
    a.write("a.md", b"m1\n");
    a.commit();

    a.run(&["branch", "create", "feature", "--checkout"]);
    assert!(a.run(&["branch", "list"]).contains("feature"));
    a.write("a.md", b"b2\n");
    a.write("only-branch.md", b"x\n");
    a.commit();
    assert_eq!(a.read_str("a.md").as_deref(), Some("b2\n"));

    // main is isolated.
    a.run(&["branch", "checkout", "main"]);
    assert_eq!(a.read_str("a.md").as_deref(), Some("m1\n"));
    assert!(!a.exists("only-branch.md"));

    // back to the branch by name.
    a.run(&["branch", "checkout", "feature"]);
    assert_eq!(a.read_str("a.md").as_deref(), Some("b2\n"));
    assert!(a.exists("only-branch.md"));

    // delete (from main; deleting HEAD auto-checks-out main anyway).
    a.run(&["branch", "checkout", "main"]);
    a.run(&["branch", "delete", "feature"]);
    assert!(!a.run(&["branch", "list"]).contains("feature"));
}

#[test]
fn branches_propagate_through_hub() {
    // A creates a branch + edits on it; a fresh clone learns the branch and can
    // check it out to its isolated state — every branch syncs, not just HEAD.
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    a.write("a.md", b"v1\n");
    a.commit();
    a.run(&["branch", "create", "feature", "--checkout"]);
    a.write("a.md", b"v2\n");
    a.write("feat-only.md", b"x\n");
    a.commit();
    a.sync(&url, Some(SECRET));

    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));

    // Branch propagation through a store-and-forward hub is asynchronous: A's
    // oneshot push can return before the hub has fully served it to B's clone.
    // Re-sync until B has learned the branch (bounded) — a single round is
    // timing-fragile under parallel CI load (the harness lesson the other
    // networked tests already encode). Convergence, not a one-shot, is the gate.
    let learned = wait_until(Duration::from_secs(15), || {
        b.sync(&url, Some(SECRET));
        b.run(&["branch", "list"]).contains("feature")
    });
    assert!(learned, "branch synced to the clone");

    // B is on main with main's state.
    assert_eq!(b.read_str("a.md").as_deref(), Some("v1\n"));
    assert!(!b.exists("feat-only.md"));

    // B checks out the synced branch and converges its isolated state (re-sync
    // until the branch's content rows have caught up too).
    b.run(&["branch", "checkout", "feature"]);
    let converged = wait_until(Duration::from_secs(15), || {
        b.sync(&url, Some(SECRET));
        b.run(&["branch", "checkout", "feature"]);
        b.read_str("a.md").as_deref() == Some("v2\n") && b.exists("feat-only.md")
    });
    assert!(converged, "branch content converged on B (a.md={:?})", b.read_str("a.md"));
}
