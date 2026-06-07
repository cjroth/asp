//! *Topology:* relay/hub forward-then-merge; two clones through one relay;
//! transitive relay trust (two writers converge through a single relay without
//! either enumerating the other's key — only the relay holds the auth secret).

use asp_e2e::{temp_root, Hub, Node};

const SECRET: &str = "relay-secret";

#[test]
fn two_clones_through_one_relay() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    a.write("doc.md", b"v1\n");
    a.sync(&url, Some(SECRET));

    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));
    let c = Node::new(root.path(), "C");
    c.clone_from(&url, Some(SECRET));

    // A publishes through the relay; both B and C receive it (store-and-forward).
    a.write("doc.md", b"v1\nv2\n");
    a.write("from-a.md", b"a\n");
    a.commit();
    a.sync(&url, Some(SECRET));
    b.sync(&url, Some(SECRET));
    c.sync(&url, Some(SECRET));

    assert_eq!(b.read_str("doc.md").as_deref(), Some("v1\nv2\n"));
    assert_eq!(c.read_str("doc.md").as_deref(), Some("v1\nv2\n"));
    assert_eq!(b.read_str("from-a.md").as_deref(), Some("a\n"));
    assert_eq!(c.read_str("from-a.md").as_deref(), Some("a\n"));
}

#[test]
fn transitive_relay_trust_two_writers_one_relay() {
    let root = temp_root();
    // Only the relay holds the AUTH_KEY secret; A and C never exchange keys.
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    a.write("a.md", b"written by A\n");
    a.sync(&url, Some(SECRET)); // A enrolls at the relay

    let c = Node::new(root.path(), "C");
    c.clone_from(&url, Some(SECRET)); // C enrolls at the relay, pulls A's data
    assert_eq!(c.read_str("a.md").as_deref(), Some("written by A\n"));

    // C writes; A receives it through the relay — neither authorized the other.
    c.write("c.md", b"written by C\n");
    c.commit();
    c.sync(&url, Some(SECRET));
    a.sync(&url, Some(SECRET));
    assert_eq!(a.read_str("c.md").as_deref(), Some("written by C\n"), "writers converge via the relay");
}
