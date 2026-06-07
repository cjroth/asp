//! *Sync core:* clone + full catch-up; offline → reconnect catch-up via version
//! vectors (sends exactly what's missing, observed as convergence after a gap).

use asp_e2e::{temp_root, Hub, Node};

const SECRET: &str = "k";

#[test]
fn clone_full_catchup() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    for i in 0..10 {
        a.write(&format!("f{i}.md"), format!("content {i}\n").as_bytes());
    }
    a.commit();
    a.sync(&url, Some(SECRET));

    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));
    for i in 0..10 {
        assert_eq!(b.read_str(&format!("f{i}.md")).as_deref(), Some(&*format!("content {i}\n")));
    }
}

#[test]
fn offline_then_reconnect_catchup() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    a.write("base.md", b"v1\n");
    a.sync(&url, Some(SECRET));

    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));
    assert_eq!(b.read_str("base.md").as_deref(), Some("v1\n"));

    // A goes "offline" and accumulates several changes locally.
    for i in 0..5 {
        a.write(&format!("offline{i}.md"), format!("o{i}\n").as_bytes());
    }
    a.write("base.md", b"v2\n");
    a.commit();
    let rows_before = a.rows();
    assert!(rows_before >= 6);

    // Reconnect: only the missing rows flow (version vectors), and B converges.
    a.sync(&url, Some(SECRET));
    b.sync(&url, Some(SECRET));
    assert_eq!(b.read_str("base.md").as_deref(), Some("v2\n"));
    for i in 0..5 {
        assert_eq!(b.read_str(&format!("offline{i}.md")).as_deref(), Some(&*format!("o{i}\n")));
    }
    // Both hold the same number of rows after catch-up (nothing lost or duplicated).
    assert_eq!(a.rows(), b.rows());
}
