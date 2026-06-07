//! *Delete truth table:* v1 remove-wins — a concurrent edit does not resurrect a
//! deleted file (kept in history, recoverable); delete vs concurrent rename also
//! resolves to removed.

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
fn delete_dominates_concurrent_edit() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    a.write("doc.md", b"important\n");
    a.sync(&url, Some(SECRET));
    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));

    // A deletes; B edits the same file concurrently.
    a.remove("doc.md");
    a.commit();
    b.write("doc.md", b"important\nplus more\n");
    b.commit();
    converge(&[&a, &b], &url);

    // Remove-wins: the file is gone on both, and the edit did not resurrect it.
    assert!(!a.exists("doc.md"), "delete dominates on A");
    assert!(!b.exists("doc.md"), "delete dominates on B");
    // The losing edit is retained in history (the row exists), recoverable.
    assert_eq!(a.rows(), b.rows());
    assert!(a.rows() >= 3, "create + delete + edit all recorded");
}

#[test]
fn delete_dominates_concurrent_rename() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    a.write("note.md", b"some note with enough content\n");
    a.sync(&url, Some(SECRET));
    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));

    a.remove("note.md");
    a.commit();
    b.rename("note.md", "renamed.md");
    b.commit();
    converge(&[&a, &b], &url);

    assert!(!a.exists("note.md") && !b.exists("note.md"));
    assert!(!a.exists("renamed.md") && !b.exists("renamed.md"), "delete wins over the rename");
}
