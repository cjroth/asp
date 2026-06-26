//! `asp watch --listen --relay`: the all-in-one box — it serves its own vault AND
//! co-hosts an iroh relay in the same process. Two checks:
//!   1. a fresh node clones content out of the combined box (serve path intact);
//!   2. the co-hosted relay is actually bound and accepting connections.
//!
//! Hermetic same-host (`ASP_NO_RELAY=1`): the clone's data path is a direct
//! loopback dial, so this asserts the combined mode is wired correctly and
//! doesn't regress serving. The relay is exercised as a real forwarding path
//! over the network in the fly integration test (`tests/fly_integration.rs`).

use asp_e2e::{temp_root, Hub, Node};
use std::time::{Duration, Instant};

const SECRET: &str = "all-in-one-secret";

/// Grab an ephemeral TCP port for the co-hosted relay so parallel tests (and
/// back-to-back runs with a port still in TIME_WAIT) never collide on :8080.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

#[test]
fn all_in_one_listen_relay_serves_clone() {
    let root = temp_root();
    let bind = format!("127.0.0.1:{}", free_port());
    // The all-in-one hub: --listen (serve) + --relay (co-host a relay).
    let hub = Hub::start(root.path(), "vault", Some(SECRET), &["--relay", "--relay-listen-addr", &bind]);
    let url = hub.url();

    // A node pushes content into the all-in-one box.
    let a = Node::new(root.path(), "A");
    a.init();
    a.write("hosted.md", b"served by the all-in-one box\n");
    a.commit();
    a.sync(&url, Some(SECRET));

    // A fresh node clones it back out of the same box.
    let b = Node::new(root.path(), "B");
    b.clone_from(&url, Some(SECRET));
    assert_eq!(
        b.read_str("hosted.md").as_deref(),
        Some("served by the all-in-one box\n"),
        "clone over `asp watch --listen --relay` should pull the hosted content"
    );
}

#[test]
fn co_hosted_relay_is_actually_listening() {
    let root = temp_root();
    let port = free_port();
    let bind = format!("127.0.0.1:{port}");
    // Start the all-in-one box and confirm the in-process relay really bound its
    // port (not just that direct-dial worked) — a TCP connect must succeed.
    let _hub = Hub::start(root.path(), "vault", Some(SECRET), &["--relay", "--relay-listen-addr", &bind]);

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut connected = false;
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            connected = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(connected, "co-hosted relay should be accepting TCP connections on {bind}");
}
