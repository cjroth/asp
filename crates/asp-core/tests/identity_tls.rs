//! Identity (ed25519 sign/verify, ssh-key encoding) and the TLS material
//! (self-signed cert generation, channel-binding fingerprint, rustls configs).
//! Trust rides on these; they were partially covered.

use asp_core::identity::{parse_ssh_pubkey, ssh_pubkey_string, verify_detached};
use asp_core::{tls, Identity};

#[test]
fn sign_verify_and_ssh_pubkey_roundtrip() {
    let id = Identity::from_seed(&[42; 32]);
    let node = id.node_id();

    let msg = b"the signed handshake transcript";
    let sig = id.sign(msg);
    assert!(verify_detached(&node, msg, &sig).is_ok(), "valid signature verifies");
    assert!(verify_detached(&node, b"a different message", &sig).is_err(), "tampered message fails");
    assert!(verify_detached(&node, msg, &[0u8; 64]).is_err(), "garbage signature fails");

    // The ssh-ed25519 string round-trips back to the same node id.
    let ssh = id.to_ssh_string();
    assert!(ssh.starts_with("ssh-ed25519 "));
    assert_eq!(parse_ssh_pubkey(&ssh).expect("parse"), node);
    assert!(parse_ssh_pubkey("ssh-rsa AAAA notanedkey").is_none());
    assert!(parse_ssh_pubkey("total garbage").is_none());

    // from_seed is deterministic; generate is not.
    assert_eq!(Identity::from_seed(&[42; 32]).node_id(), node);
    assert_ne!(Identity::generate().node_id(), node);
    assert_eq!(id.seed(), [42; 32]);
    assert!(ssh_pubkey_string(&node, "comment").starts_with("ssh-ed25519 "));
}

#[test]
fn tls_cert_generation_fingerprint_and_configs() {
    let (cert, key) = tls::generate_self_signed().unwrap();
    assert!(!cert.is_empty() && !key.is_empty());

    // The channel-binding fingerprint is a stable 32-byte SHA-256 of the cert.
    let fp = tls::cert_fingerprint(&cert);
    assert_eq!(fp.len(), 32);
    assert_eq!(tls::cert_fingerprint(&cert), fp, "same cert → same fingerprint");
    let (cert2, _key2) = tls::generate_self_signed().unwrap();
    assert_ne!(tls::cert_fingerprint(&cert2), fp, "a fresh cert → different fingerprint");

    // Both rustls configs build from the generated material.
    let _server = tls::server_config(cert, key).unwrap();
    let _client = tls::client_config_accept_any();
}

#[test]
fn tls_load_or_generate_is_stable_across_calls() {
    let dir = tempfile::tempdir().unwrap();
    let (c1, k1) = tls::load_or_generate(dir.path()).unwrap();
    // Second call must reload the persisted cert, not mint a new one (else every
    // restart would change the hub's channel-binding fingerprint).
    let (c2, k2) = tls::load_or_generate(dir.path()).unwrap();
    assert_eq!(c1, c2, "cert is stable across calls");
    assert_eq!(k1, k2, "key is stable across calls");
}
