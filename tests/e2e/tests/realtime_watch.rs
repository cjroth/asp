//! *Realtime:* the primary long-running `asp watch` command — debounced capture,
//! optimistic real-time push, relay forward — propagates a change end-to-end with
//! no manual sync, and self-writes don't echo-storm.

use asp_e2e::{temp_root, wait_until, Hub, Node, Watcher};
use std::time::Duration;

const SECRET: &str = "k";

#[test]
fn change_propagates_in_realtime_through_relay() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    // Establish a shared vault, then both peers run the realtime watcher.
    let a = Node::new(root.path(), "A");
    a.init();
    a.write("seed.md", b"seed\n");
    a.sync(&url, Some(SECRET));
    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));

    let _wa = Watcher::start(&a, &url, Some(SECRET), false);
    let _wb = Watcher::start(&b, &url, Some(SECRET), false);

    // A new edit on A should reach B with no manual sync.
    a.write("live.md", b"typed on A\n");

    let got = wait_until(Duration::from_secs(20), || {
        b.read_str("live.md").as_deref() == Some("typed on A\n")
    });
    assert!(got, "realtime change did not propagate to B in time");

    // A second edit (modify) propagates too.
    a.write("live.md", b"typed on A\nand more\n");
    let got2 = wait_until(Duration::from_secs(20), || {
        b.read_str("live.md").as_deref() == Some("typed on A\nand more\n")
    });
    assert!(got2, "realtime modify did not propagate to B");
}

#[test]
fn realtime_bidirectional() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    a.write("seed.md", b"seed\n");
    a.sync(&url, Some(SECRET));
    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));

    let _wa = Watcher::start(&a, &url, Some(SECRET), false);
    let _wb = Watcher::start(&b, &url, Some(SECRET), false);

    a.write("from-a.md", b"A says hi\n");
    b.write("from-b.md", b"B says hi\n");

    let a_to_b = wait_until(Duration::from_secs(20), || b.exists("from-a.md"));
    let b_to_a = wait_until(Duration::from_secs(20), || a.exists("from-b.md"));
    assert!(a_to_b, "A→B realtime failed");
    assert!(b_to_a, "B→A realtime failed");
    assert_eq!(b.read_str("from-a.md").as_deref(), Some("A says hi\n"));
    assert_eq!(a.read_str("from-b.md").as_deref(), Some("B says hi\n"));
}
