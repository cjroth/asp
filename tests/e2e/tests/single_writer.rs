//! *Ordering & re-fold / single-writer:* two replicas of one vault on the **same
//! device** (sharing the `~/.asp` device key) must still converge. Each vault has
//! its own per-vault authoring `site_id`, so their concurrent edits don't collide
//! on `(site_id, seq)` — which would silently defeat version-vector catch-up (the
//! reported "edits made while disconnected never reconcile" bug).

use asp_e2e::{temp_root, Hub, Node};

const SECRET: &str = "k";

#[test]
fn replicas_sharing_a_device_identity_converge() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    // A and B are two folders on the SAME machine — they share the device
    // identity (`$ASP_HOME`), exactly like two vaults under one `~/.asp`.
    let a = Node::new(root.path(), "A");
    let mut b = Node::new(root.path(), "B");
    b.home = a.home.clone();

    a.init();
    a.write("doc.md", b"base line\n");
    a.sync(&url, Some(SECRET));
    b.clone_from(&url, Some(SECRET));

    // Despite the shared device key, each vault has a distinct authoring site id.
    let sa = std::fs::read_to_string(a.dir.join(".asp/site_id")).unwrap();
    let sb = std::fs::read_to_string(b.dir.join(".asp/site_id")).unwrap();
    assert_ne!(sa.trim(), sb.trim(), "each vault forks its own site_id");

    // Concurrent edits to the same line while both are disconnected.
    a.write("doc.md", b"from-A\n");
    b.write("doc.md", b"from-B\n");
    a.commit();
    b.commit();
    for _ in 0..2 {
        a.sync(&url, Some(SECRET));
        b.sync(&url, Some(SECRET));
    }

    // They converge — the catch-up exchanged both edits (it would have missed them
    // if both rows shared one `(site_id, seq)`).
    assert_eq!(a.read_str("doc.md"), b.read_str("doc.md"), "replicas converge despite shared device key");
    let r = a.read_str("doc.md").unwrap();
    assert!(r == "from-A\n" || r == "from-B\n", "resolved to one side: {r:?}");
}
