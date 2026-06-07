//! *Ordering & re-fold:* the Lamport clock is durable across process restarts
//! (each `asp` invocation is a fresh process); a late-arriving concurrent row
//! folds at its position and the result is identical regardless of arrival order.

use asp_e2e::{temp_root, Hub, Node};

const SECRET: &str = "k";

#[test]
fn lamport_is_durable_across_process_restarts() {
    let root = temp_root();
    let a = Node::new(root.path(), "A");
    a.init();

    // Each commit is a separate `asp` process; the Lamport clock is derived from
    // the durable log, so it never resets.
    a.write("f.md", b"v1\n");
    a.commit();
    a.write("f.md", b"v2\n");
    a.commit();
    a.write("g.md", b"g1\n");
    a.commit();

    let log: serde_json::Value = serde_json::from_str(&a.run(&["log", "--json"])).unwrap();
    let lamports: Vec<u64> = log.as_array().unwrap().iter().map(|r| r["lamport"].as_u64().unwrap()).collect();
    assert!(lamports.len() >= 3);
    // Strictly increasing across restarts (each new local row = max(observed)+1).
    for w in lamports.windows(2) {
        assert!(w[1] > w[0], "lamport monotonic & persisted across restarts: {lamports:?}");
    }
    // Per-device seq is dense 0,1,2,...
    let seqs: Vec<u64> = log.as_array().unwrap().iter().map(|r| r["seq"].as_u64().unwrap()).collect();
    assert_eq!(seqs, (0..seqs.len() as u64).collect::<Vec<_>>(), "dense per-device seq");
}

#[test]
fn arrival_order_does_not_change_converged_state() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    a.write("doc.md", b"one\ntwo\nthree\nfour\nfive\n");
    a.sync(&url, Some(SECRET));
    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));

    // Two concurrent edits on different lines (separated by unchanged context).
    // Deliver B before A on the relay this run.
    a.write("doc.md", b"one\nSECOND-a\nthree\nfour\nfive\n");
    b.write("doc.md", b"one\ntwo\nthree\nFOURTH-b\nfive\n");
    a.commit();
    b.commit();
    b.sync(&url, Some(SECRET)); // B arrives first
    a.sync(&url, Some(SECRET));
    b.sync(&url, Some(SECRET));
    a.sync(&url, Some(SECRET));

    let converged = a.read_str("doc.md").unwrap();
    assert_eq!(converged, b.read_str("doc.md").unwrap());
    // Both disjoint edits survive regardless of which arrived first.
    assert!(converged.contains("SECOND-a") && converged.contains("FOURTH-b"), "got {converged:?}");
}
