//! Following a peer (§Sync): a freshly-`init`'d folder with no local content is
//! *pristine* and adopts the peer's vault on connect (so "init then follow" Just
//! Works), while a folder that already has its own content is a separate vault and
//! must not silently merge — the mismatch is a clear, actionable error.

use asp_e2e::{temp_root, Hub, Node};

const SECRET: &str = "k";

#[test]
fn pristine_init_then_sync_adopts_the_peer_vault() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    a.write("doc.md", b"from A\n");
    a.sync(&url, Some(SECRET));

    // B is `init`'d separately (its own vault id) but has committed nothing — so
    // it's pristine and adopts the peer's vault, catching up A's content. No clone
    // required.
    let b = Node::new(root.path(), "B");
    b.init();
    b.sync(&url, Some(SECRET));
    assert_eq!(b.read_str("doc.md").as_deref(), Some("from A\n"), "pristine follower adopts + catches up");
}

#[test]
fn separate_populated_vaults_do_not_silently_merge() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some(SECRET), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    a.write("a.md", b"from A\n");
    a.sync(&url, Some(SECRET)); // the hub adopts A's vault

    // C is an independent, *populated* vault — different vault id. Syncing must
    // fail loudly (not silently no-op), guiding the user to clone.
    let c = Node::new(root.path(), "C");
    c.init();
    c.write("c.md", b"independent\n");
    c.commit();
    let (ok, _out, err) = c.try_sync(&url, Some(SECRET));
    assert!(!ok, "two separate populated vaults must not merge");
    assert!(err.contains("different vault"), "the error names the cause: {err}");
    // C kept its own content; nothing from A leaked in.
    assert!(!c.exists("a.md"));
    assert_eq!(c.read_str("c.md").as_deref(), Some("independent\n"));
}
