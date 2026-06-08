//! *Merge:* disjoint concurrent text edits both survive; same-region text
//! resolves deterministically (clean, no markers); code same-region conflict is
//! surfaced byte-deterministically; binary whole-file LWW. All via real nodes
//! converging through a relay.

use asp_e2e::{temp_root, Hub, Node};

const SECRET: &str = "k";

/// Sync every node through the relay until quiescent (two rounds suffice for a
/// single hop: push, then pull-back).
fn converge(nodes: &[&Node], url: &str) {
    for _ in 0..2 {
        for n in nodes {
            n.sync(url, Some(SECRET));
        }
    }
}

/// Build a base file on A, clone B — both share it.
fn setup(root: &tempfile::TempDir, hub: &Hub, base_path: &str, base: &[u8]) -> (Node, Node) {
    let url = hub.url();
    let a = Node::new(root.path(), "A");
    a.init();
    a.write(base_path, base);
    a.sync(&url, Some(SECRET));
    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));
    (a, b)
}

#[test]
fn disjoint_text_edits_both_survive() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();
    let (a, b) = setup(&root, &hub, "doc.md", b"a\nb\nc\n");

    a.write("doc.md", b"A\nb\nc\n"); // edit line 1
    b.write("doc.md", b"a\nb\nC\n"); // edit line 3
    a.commit();
    b.commit();
    converge(&[&a, &b], &url);

    assert_eq!(a.read_str("doc.md"), b.read_str("doc.md"), "A and B converge");
    assert_eq!(a.read_str("doc.md").as_deref(), Some("A\nb\nC\n"), "both disjoint edits survive");
}

#[test]
fn same_region_text_resolves_clean_no_markers() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();
    let (a, b) = setup(&root, &hub, "doc.md", b"original\n");

    a.write("doc.md", b"from-A\n");
    b.write("doc.md", b"from-B\n");
    a.commit();
    b.commit();
    converge(&[&a, &b], &url);

    let ra = a.read_str("doc.md").unwrap();
    assert_eq!(ra, b.read_str("doc.md").unwrap(), "converge to one clean version");
    assert!(!ra.contains("<<<<<<<"), "text must never carry conflict markers");
    assert!(ra == "from-A\n" || ra == "from-B\n", "resolves to one side, got {ra:?}");
}

#[test]
fn code_same_region_conflict_is_surfaced_deterministically() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();
    let (a, b) = setup(&root, &hub, "main.rs", b"fn x() {\n    todo!()\n}\n");

    a.write("main.rs", b"fn x() {\n    return 1;\n}\n");
    b.write("main.rs", b"fn x() {\n    return 2;\n}\n");
    a.commit();
    b.commit();
    converge(&[&a, &b], &url);

    let ra = a.read_str("main.rs").unwrap();
    assert_eq!(ra, b.read_str("main.rs").unwrap(), "code conflict converges byte-identically");
    assert!(ra.contains("<<<<<<< ASP:A"), "code conflict is surfaced with markers: {ra:?}");
    assert!(ra.contains(">>>>>>> ASP:B"));
    // Both function bodies are present for the agent to resolve.
    assert!(ra.contains("return 1;") && ra.contains("return 2;"));
}

#[test]
fn binary_whole_file_last_writer_wins() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();
    let (a, b) = setup(&root, &hub, "blob.bin", &[0u8, 1, 2, 3, 0, 9]);

    a.write("blob.bin", &[0u8, 1, 2, 3, 0, 0xAA]);
    b.write("blob.bin", &[0u8, 1, 2, 3, 0, 0xBB]);
    a.commit();
    b.commit();
    converge(&[&a, &b], &url);

    assert_eq!(a.read("blob.bin"), b.read("blob.bin"), "binary converges (one whole file wins)");
    let r = a.read("blob.bin").unwrap();
    assert!(r == vec![0, 1, 2, 3, 0, 0xAA] || r == vec![0, 1, 2, 3, 0, 0xBB]);
}
