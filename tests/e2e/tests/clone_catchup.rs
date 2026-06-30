//! *Sync core:* clone + full catch-up; offline → reconnect catch-up via version
//! vectors (sends exactly what's missing, observed as convergence after a gap).

use asp_e2e::{temp_root, wait_until, Hub, Node};
use std::time::Duration;

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
fn clone_pins_the_listener_as_a_peer() {
    // §CLI `asp clone`: pin the listener's NodeId and record the source URL as a
    // peer (git's `origin`).
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    a.write("x.md", b"x\n");
    a.sync(&url, Some(SECRET));

    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));
    let st = b.status_json();
    let peers = st["peers"].as_array().expect("peers array");
    assert_eq!(peers.len(), 1, "clone pinned exactly one peer");
    assert_eq!(peers[0]["url"].as_str(), Some(url.as_str()), "the source URL is recorded");
    assert!(peers[0]["node_id"].as_str().unwrap().len() >= 16, "the listener NodeId is pinned");
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
    // Push-through a store-and-forward hub is asynchronous (A's oneshot can return
    // before the hub serves it to B), so converge with a bounded re-sync rather
    // than a single round — a one-shot assert is timing-fragile under parallel CI
    // load. Convergence is the invariant; how many rounds it takes is not.
    a.sync(&url, Some(SECRET));
    let converged = wait_until(Duration::from_secs(20), || {
        b.sync(&url, Some(SECRET));
        b.read_str("base.md").as_deref() == Some("v2\n")
            && (0..5).all(|i| b.read_str(&format!("offline{i}.md")).as_deref() == Some(&*format!("o{i}\n")))
            && a.rows() == b.rows()
    });
    assert!(
        converged,
        "B did not converge: base.md={:?} rows a={} b={}",
        b.read_str("base.md"),
        a.rows(),
        b.rows()
    );
}
