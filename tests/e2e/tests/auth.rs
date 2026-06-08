//! *Auth (pubkey):* `authorized_keys`-table admission; AUTH_KEY enrollment
//! (Bearer; 401 on mismatch, no fall-through; absent header proceeds for
//! already-enrolled peers); TOFU bounded to the empty-set window; `--no-tofu`;
//! explicit authorize/revoke gating.

use asp_e2e::{admin_cmd, temp_root, Hub, Node};

#[test]
fn auth_key_enrollment_wrong_key_401_then_enrolled_without_key() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", Some("right-secret"), &[]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    a.write("x.md", b"hello\n");

    // Wrong key → rejected at the upgrade (HTTP 401), no fall-through.
    let (ok, _, err) = a.try_sync(&url, Some("wrong-secret"));
    assert!(!ok, "mismatched auth key must be rejected");
    assert!(err.to_lowercase().contains("401") || err.to_lowercase().contains("error"), "got: {err}");

    // Correct key → enrolled and synced.
    a.sync(&url, Some("right-secret"));

    // From the next connection on, the enrolled peer connects WITHOUT the secret
    // (absent header proceeds for already-enrolled peers). `sync` panics on
    // failure, so reaching here proves the keyless reconnect was admitted.
    a.write("x.md", b"hello again\n");
    a.commit();
    a.sync(&url, None);
}

#[test]
fn tofu_is_bounded_to_the_empty_set_window() {
    let root = temp_root();
    // No auth key configured → TOFU is available while the set is empty.
    let hub = Hub::start(root.path(), "hub", None, &[]);
    let url = hub.url();

    // First peer is trusted-on-first-use and enrolled.
    let a = Node::new(root.path(), "A");
    a.init();
    a.write("seed.md", b"seed\n");
    a.sync(&url, None);
    assert_eq!(a.read_str("seed.md").as_deref(), Some("seed\n"));

    // The set is now non-empty → a *different* device is refused (TOFU closed).
    let b = Node::new(root.path(), "B");
    let (ok, _, err) = b.try_clone_from(&url, None);
    assert!(!ok, "second peer must be denied once the TOFU window closed: {err}");
}

#[test]
fn no_tofu_refuses_the_first_peer() {
    let root = temp_root();
    let hub = Hub::start(root.path(), "hub", None, &["--no-tofu"]);
    let url = hub.url();

    let a = Node::new(root.path(), "A");
    a.init();
    a.write("x.md", b"hi\n");
    let (ok, _, err) = a.try_sync(&url, None);
    assert!(!ok, "with --no-tofu and an empty set + no auth key, admission is refused: {err}");
}

#[test]
fn explicit_authorize_admits_unknown_denied() {
    let root = temp_root();
    let a = Node::new(root.path(), "A");
    a.init();
    a.write("doc.md", b"authorized content\n");
    let a_key = a.key();

    // Pre-seed the hub's admission table with A's key, then start it with TOFU off.
    let (ok, _, err) = admin_cmd(root.path(), "hub", &["authorize", &a_key]);
    assert!(ok, "authorize failed: {err}");
    let hub = Hub::start(root.path(), "hub", None, &["--no-tofu"]);
    let url = hub.url();

    // A (authorized) is admitted.
    a.sync(&url, None);

    // B (never authorized, no auth key, no TOFU) is denied.
    let b = Node::new(root.path(), "B");
    let (ok, _, err) = b.try_clone_from(&url, None);
    assert!(!ok, "unauthorized peer must be denied: {err}");
}

#[test]
fn auth_list_and_revoke_via_cli() {
    let root = temp_root();
    let peer = Node::new(root.path(), "peer");
    peer.init();
    let peer_key = peer.key();

    admin_cmd(root.path(), "hub", &["authorize", &peer_key, "--ttl", "30d"]);
    let (_, out, _) = admin_cmd(root.path(), "hub", &["auth", "list", "--json"]);
    assert!(out.contains("\"source\""), "auth list --json should show entries: {out}");
    assert!(out.contains("expires_at"));

    // Revoke removes it.
    let (ok, _, _) = admin_cmd(root.path(), "hub", &["revoke", &peer_key]);
    assert!(ok);
    let (_, out2, _) = admin_cmd(root.path(), "hub", &["auth", "list"]);
    assert!(out2.contains("no authorized keys"), "after revoke the set is empty: {out2}");
}

#[test]
fn auth_extend_lengthens_expiry() {
    let root = temp_root();
    let peer = Node::new(root.path(), "peer");
    peer.init();
    let peer_key = peer.key();

    admin_cmd(root.path(), "hub", &["authorize", &peer_key, "--ttl", "30d"]);
    let exp1 = expiry_of(root.path(), &peer_key);

    let (ok, _, err) = admin_cmd(root.path(), "hub", &["auth", "extend", &peer_key, "1y"]);
    assert!(ok, "auth extend failed: {err}");
    let exp2 = expiry_of(root.path(), &peer_key);
    assert!(exp2 > exp1, "auth extend lengthens the expiry ({exp1} -> {exp2})");
}

fn expiry_of(root: &std::path::Path, pubkey: &str) -> i64 {
    let node_hex = asp_core::identity::parse_ssh_pubkey(pubkey).unwrap().to_hex();
    let (_, out, _) = admin_cmd(root, "hub", &["auth", "list", "--json"]);
    let arr: serde_json::Value = serde_json::from_str(&out).unwrap();
    for k in arr.as_array().unwrap() {
        if k["node_id"].as_str() == Some(node_hex.as_str()) {
            return k["expires_at"].as_i64().unwrap_or(0);
        }
    }
    panic!("key not found in auth list: {out}");
}
