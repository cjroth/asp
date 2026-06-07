//! *Headline gate (determinism):* N real nodes, concurrent edits across disjoint
//! and same regions, converge to **identical** materialized state *and* an
//! identical derived `main` SHA — the convergence/determinism guarantee against
//! spawned processes (not an in-process shortcut).

use asp_e2e::{temp_root, Hub, Node};

const SECRET: &str = "k";

fn converge(nodes: &[&Node], url: &str) {
    // Three rounds so changes fully propagate across the single relay hop in any
    // authoring order.
    for _ in 0..3 {
        for n in nodes {
            n.sync(url, Some(SECRET));
        }
    }
}

#[test]
fn three_nodes_converge_identically() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    // Lines `hN` are shared context; A/B/C each edit a *different* line, each
    // separated by unchanged context, so all three edits survive (line-level
    // 3-way), then converge.
    a.write("shared.md", b"h0\nl-a\nh1\nl-b\nh2\nl-c\nh3\n");
    a.write("a-only.md", b"alpha\n");
    a.sync(&url, Some(SECRET));

    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));
    let c = Node::new(root.path(), "C");
    c.clone_from(&url, Some(SECRET));

    // Concurrent disjoint-line edits + each adds its own file.
    a.write("shared.md", b"h0\nA-edit\nh1\nl-b\nh2\nl-c\nh3\n");
    a.write("a2.md", b"a2\n");
    b.write("shared.md", b"h0\nl-a\nh1\nB-edit\nh2\nl-c\nh3\n");
    b.write("b2.md", b"b2\n");
    c.write("shared.md", b"h0\nl-a\nh1\nl-b\nh2\nC-edit\nh3\n");
    c.write("c2.md", b"c2\n");
    a.commit();
    b.commit();
    c.commit();

    converge(&[&a, &b, &c], &url);

    // All three converge byte-identically...
    let sa = a.read_str("shared.md").unwrap();
    assert_eq!(sa, b.read_str("shared.md").unwrap(), "A == B");
    assert_eq!(sa, c.read_str("shared.md").unwrap(), "A == C");
    // ...and every disjoint-line edit survives (line-level 3-way).
    assert!(
        sa.contains("A-edit") && sa.contains("B-edit") && sa.contains("C-edit"),
        "all disjoint edits survive: {sa:?}"
    );
    // ...every node's per-node files are present everywhere.
    for n in [&a, &b, &c] {
        assert_eq!(n.read_str("a2.md").as_deref(), Some("a2\n"));
        assert_eq!(n.read_str("b2.md").as_deref(), Some("b2\n"));
        assert_eq!(n.read_str("c2.md").as_deref(), Some("c2\n"));
    }
    // ...and the derived main SHA is identical on all three.
    let ha = a.head();
    assert_eq!(ha, b.head(), "derived SHA A == B");
    assert_eq!(ha, c.head(), "derived SHA A == C");
    assert_eq!(a.rows(), c.rows(), "all hold the same row set");
}
