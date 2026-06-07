//! *Sync core:* two-peer create / modify / delete through a listening relay.
//! Every assertion is against real spawned `asp` processes converging.

use asp_e2e::{temp_root, Hub, Node};

const SECRET: &str = "s3cr3t";

#[test]
fn create_modify_delete_converge_through_relay() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    a.write("notes/todo.md", b"buy milk\n");
    a.sync(&url, Some(SECRET));

    // B bootstraps from the relay and sees A's file.
    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));
    assert_eq!(b.read_str("notes/todo.md").as_deref(), Some("buy milk\n"));

    // A modifies; propagate A -> hub -> B.
    a.write("notes/todo.md", b"buy milk\nand eggs\n");
    a.sync(&url, Some(SECRET));
    b.sync(&url, Some(SECRET));
    assert_eq!(b.read_str("notes/todo.md").as_deref(), Some("buy milk\nand eggs\n"));

    // B deletes; propagate B -> hub -> A. Delete is an explicit ordered row.
    b.remove("notes/todo.md");
    b.commit();
    b.sync(&url, Some(SECRET));
    a.sync(&url, Some(SECRET));
    assert!(!a.exists("notes/todo.md"), "delete must propagate and remove the file on A");
    assert!(!b.exists("notes/todo.md"));
}

#[test]
fn empty_directories_and_nested_paths() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    a.write("a/b/c/deep.md", b"deep\n");
    a.write("top.md", b"top\n");
    a.sync(&url, Some(SECRET));

    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));
    assert_eq!(b.read_str("a/b/c/deep.md").as_deref(), Some("deep\n"));
    assert_eq!(b.read_str("top.md").as_deref(), Some("top\n"));
}

#[test]
fn binary_file_whole_file_sync() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    let blob: Vec<u8> = (0u8..=255).cycle().take(2000).collect();
    a.write("img.bin", &blob);
    a.sync(&url, Some(SECRET));

    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));
    assert_eq!(b.read("img.bin"), Some(blob));
}
