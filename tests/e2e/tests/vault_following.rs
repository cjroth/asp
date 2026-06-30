//! Following a peer (§Sync): a freshly-`init`'d folder with no local content is
//! *pristine* and adopts the peer's vault on connect (so "init then follow" Just
//! Works), while a folder that already has its own content is a separate vault and
//! must not silently merge — the mismatch is a clear, actionable error.

use asp_e2e::{admin_cmd, temp_root, wait_until, Hub, Node};
use std::time::Duration;

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

    // The hub adopts A's vault asynchronously; wait until it has, so a
    // still-pristine hub doesn't race-adopt C instead (an intra-test race that
    // flakes under parallel CI load — the negative case can't trigger then).
    let a_vault = a.status_json()["vault_id"].as_str().unwrap_or("").to_string();
    let adopted = wait_until(Duration::from_secs(15), || {
        let (ok, out, _) = admin_cmd(root.path(), "hub", &["status", "--json"]);
        ok && serde_json::from_str::<serde_json::Value>(&out)
            .ok()
            .and_then(|v| v["vault_id"].as_str().map(|s| !s.is_empty() && s == a_vault))
            .unwrap_or(false)
    });
    assert!(adopted, "hub did not adopt A's vault in time");

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
