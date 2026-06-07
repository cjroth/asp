//! *Transport:* `wss://` self-signed default end-to-end — TLS confidentiality
//! with the ed25519 handshake as the trust boundary, and the advertised
//! channel-binding fingerprint verified by the connector. Convergence over wss
//! is byte-identical to ws.

use asp_e2e::{temp_root, Hub, Node};

const SECRET: &str = "tls-secret";

#[test]
fn wss_self_signed_sync_and_clone_converge() {
    let root = temp_root();
    // Default transport: a wss:// listener with a persisted self-signed cert.
    let hub = Hub::start_tls(root.path(), "hub", Some(SECRET));
    let url = hub.url();
    assert!(url.starts_with("wss://"), "hub advertises wss: {url}");

    let a = Node::new(root.path(), "A");
    a.init();
    a.write("secure.md", b"over tls\n");
    a.sync(&url, Some(SECRET));

    // Clone over wss (connector observes the cert fingerprint == advertised
    // channel binding, signed into the handshake transcript).
    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));
    assert_eq!(b.read_str("secure.md").as_deref(), Some("over tls\n"));

    // Bidirectional edit converges over wss too.
    b.write("reply.md", b"got it\n");
    b.commit();
    b.sync(&url, Some(SECRET));
    a.sync(&url, Some(SECRET));
    assert_eq!(a.read_str("reply.md").as_deref(), Some("got it\n"));
}
