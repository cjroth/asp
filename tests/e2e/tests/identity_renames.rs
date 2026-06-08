//! *Identity & renames:* host-signal rename keeps `file_id` + edit history;
//! concurrent rename + edit both apply; concurrent same-path create splits with a
//! deterministic ` (n)` suffix; the identity-convergence headline gate.

use asp_e2e::{temp_root, Hub, Node};

const SECRET: &str = "k";

fn converge(nodes: &[&Node], url: &str) {
    for _ in 0..2 {
        for n in nodes {
            n.sync(url, Some(SECRET));
        }
    }
}

#[test]
fn rename_keeps_file_and_concurrent_edit_applies() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    a.write("notes/plan.md", b"# Plan\nstep one\nstep two\n");
    a.sync(&url, Some(SECRET));
    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));

    // A renames (host signal: real fs rename; capture infers it by content).
    a.rename("notes/plan.md", "notes/roadmap.md");
    a.commit();
    // B concurrently edits the *old* path's content (different attribute).
    b.write("notes/plan.md", b"# Plan\nstep one\nstep two\nstep three\n");
    b.commit();

    converge(&[&a, &b], &url);

    // Converge: the file lives at the new path with B's edit intact (file_id
    // identity preserved across the rename — delete+create would have lost it).
    assert!(a.exists("notes/roadmap.md") && b.exists("notes/roadmap.md"));
    assert!(!a.exists("notes/plan.md") && !b.exists("notes/plan.md"));
    assert_eq!(a.read_str("notes/roadmap.md"), b.read_str("notes/roadmap.md"));
    assert_eq!(
        a.read_str("notes/roadmap.md").as_deref(),
        Some("# Plan\nstep one\nstep two\nstep three\n"),
        "B's concurrent edit survives on the renamed file"
    );
}

#[test]
fn folder_rename_leaves_no_empty_old_dirs() {
    // Renaming a folder (moving its files to a new tree) must not leave the old,
    // now-empty directory tree behind on any node — the vacated folders are
    // rename leftovers, not intentional empty folders, so they're not preserved
    // as `dir` entities and materialize prunes them.
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    a.write("proj/note.md", b"this is the note content\n");
    a.write("proj/sub/deep.md", b"this is the deep content\n");
    a.sync(&url, Some(SECRET));
    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));
    assert!(b.exists("proj/note.md") && b.exists("proj/sub/deep.md"));

    // "Rename" the folder by moving its files into a new tree (the old proj/ and
    // proj/sub/ are left physically empty on A's disk).
    a.rename("proj/note.md", "renamed/note.md");
    a.rename("proj/sub/deep.md", "renamed/sub/deep.md");
    a.commit();
    a.sync(&url, Some(SECRET));
    b.sync(&url, Some(SECRET));

    for n in [&a, &b] {
        assert!(n.exists("renamed/note.md"), "files moved to the new folder");
        assert!(n.exists("renamed/sub/deep.md"));
        assert!(!n.dir.join("proj").exists(), "old folder tree is gone (no leftover empty dirs)");
    }
}

#[test]
fn concurrent_same_path_create_splits_with_suffix() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    // Establish a shared (empty) vault: A inits + syncs, B clones.
    let a = Node::new(root.path(), "A");
    a.init();
    a.write("seed.md", b"seed\n");
    a.sync(&url, Some(SECRET));
    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));

    // Both independently create the *same* path with different content.
    a.write("todo.md", b"from A\n");
    b.write("todo.md", b"from B\n");
    a.commit();
    b.commit();
    converge(&[&a, &b], &url);

    // Splits deterministically: one keeps todo.md, the other gets todo (1).md.
    // Both nodes agree on the assignment (identity-convergence gate).
    assert!(a.exists("todo.md") && a.exists("todo (1).md"), "A split the collision");
    assert!(b.exists("todo.md") && b.exists("todo (1).md"), "B split the collision");
    assert_eq!(a.read_str("todo.md"), b.read_str("todo.md"), "same path → same content on both");
    assert_eq!(a.read_str("todo (1).md"), b.read_str("todo (1).md"));
    // Both original contents survive somewhere (no silent loss).
    let mut got: Vec<String> = vec![a.read_str("todo.md").unwrap(), a.read_str("todo (1).md").unwrap()];
    got.sort();
    assert_eq!(got, vec!["from A\n".to_string(), "from B\n".to_string()]);
}

#[test]
fn rename_into_occupied_path_suffixes_deterministically() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    a.write("a.md", b"content of A file aaaa\n");
    a.write("b.md", b"content of B file bbbb\n");
    a.sync(&url, Some(SECRET));
    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));

    // A renames a.md -> shared.md; B renames b.md -> shared.md (concurrent,
    // into the same target path).
    a.rename("a.md", "shared.md");
    a.commit();
    b.rename("b.md", "shared.md");
    b.commit();
    converge(&[&a, &b], &url);

    // Deterministic suffixing; both nodes agree.
    assert_eq!(a.read_str("shared.md"), b.read_str("shared.md"));
    assert!(a.exists("shared (1).md") && b.exists("shared (1).md"));
    assert_eq!(a.read_str("shared (1).md"), b.read_str("shared (1).md"));
}
