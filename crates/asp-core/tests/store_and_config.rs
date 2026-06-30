//! Coverage for the persistence substrate: `config.rs` (synced vault config) and
//! the `SqliteStore` CRUD surface (peers, embeddings, snapshots, authkeys, the
//! row/version-vector queries). These were cold spots — the engine exercises a
//! slice of the store, but the standalone tables (peers/embeddings/snapshots) and
//! the genesis-immutability rule had no direct tests.

use asp_core::{AuthKey, Engine, Identity, Store, VaultConfig};

const SSH: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIN1SPB1Au9ASedCsH0QN6iz5G+cop6tuxYD8CKoRvwt2 asp";

// ── config.rs ──────────────────────────────────────────────────────────────
#[test]
fn vault_config_genesis_defaults_and_immutability() {
    let store = Store::open_memory().unwrap();
    let cfg = VaultConfig::new(&store);

    // Defaults before genesis.
    assert_eq!(cfg.tiebreak_key().unwrap(), "lamport");
    assert_eq!(cfg.default_key_ttl().unwrap(), "90d");
    assert_eq!(cfg.debounce_ms().unwrap(), 400);
    assert_eq!(cfg.vault_id().unwrap(), None);

    cfg.init_genesis("vault-xyz").unwrap();
    assert_eq!(cfg.vault_id().unwrap().as_deref(), Some("vault-xyz"));
    // init_genesis is idempotent — a second call must not overwrite the vault id.
    cfg.init_genesis("vault-OTHER").unwrap();
    assert_eq!(cfg.vault_id().unwrap().as_deref(), Some("vault-xyz"));

    // Settable knobs round-trip.
    cfg.set_default_key_ttl("1y").unwrap();
    assert_eq!(cfg.default_key_ttl().unwrap(), "1y");
    store.set_config("debounce_ms", "750").unwrap();
    assert_eq!(cfg.debounce_ms().unwrap(), 750);
    // A non-numeric debounce falls back to the default rather than erroring.
    store.set_config("debounce_ms", "not-a-number").unwrap();
    assert_eq!(cfg.debounce_ms().unwrap(), 400);

    // tiebreak is settable on an EMPTY vault…
    cfg.set_tiebreak("lamport").unwrap();
}

#[test]
fn tiebreak_is_genesis_immutable_once_rows_exist() {
    let dir = tempfile::tempdir().unwrap();
    let e = Engine::init(dir.path(), Identity::from_seed(&[2; 32])).unwrap();
    std::fs::write(dir.path().join("a.md"), b"x\n").unwrap();
    e.capture_rescan().unwrap();
    // The vault now has rows → changing the fold-parameterizing key must fail.
    let cfg = VaultConfig::new(&e.store);
    assert!(cfg.set_tiebreak("hlc").is_err(), "tiebreak must be immutable on a populated vault");
}

// ── sqlite.rs: standalone CRUD tables ────────────────────────────────────────
#[test]
fn store_peers_embeddings_snapshots_roundtrip() {
    let s = Store::open_memory().unwrap();

    // peers
    assert!(s.peers().unwrap().is_empty());
    s.add_peer("wss://hub.example", "node-abc", 100).unwrap();
    s.add_peer("wss://hub.example", "node-abc", 200).unwrap(); // upsert, not dup
    let peers = s.peers().unwrap();
    assert_eq!(peers.len(), 1);
    assert_eq!(peers[0], ("wss://hub.example".into(), "node-abc".into()));

    // embeddings (content-addressed, model-versioned)
    assert_eq!(s.get_embedding("h1", "m1").unwrap(), None);
    s.put_embedding("h1", "m1", &[1, 2, 3, 4]).unwrap();
    assert_eq!(s.get_embedding("h1", "m1").unwrap(), Some(vec![1, 2, 3, 4]));
    assert_eq!(s.get_embedding("h1", "m2").unwrap(), None); // different model

    // snapshots
    assert!(s.snapshots().unwrap().is_empty());
    s.insert_snapshot("snap1", 5, "before-refactor", "treehash", 1_700_000_000, "{\"a.md\":\"h\"}").unwrap();
    let by = s.snapshot_by_label("before-refactor").unwrap().expect("snapshot present");
    assert_eq!(by.0, "snap1");
    assert_eq!(by.2, "{\"a.md\":\"h\"}"); // manifest round-trips
    assert_eq!(s.snapshots().unwrap().len(), 1);
    assert!(s.snapshot_by_label("nope").unwrap().is_none());
}

#[test]
fn store_authkeys_crud_and_expiry() {
    let s = Store::open_memory().unwrap();
    assert!(s.authkeys_empty().unwrap());

    let k = AuthKey::from_ssh(SSH, None, false, 1000, "test").expect("parse ssh");
    let node = k.node_id.clone();
    s.insert_authkey(&k).unwrap();
    assert!(!s.authkeys_empty().unwrap());
    assert_eq!(s.authkeys().unwrap().len(), 1);
    assert!(s.authkey_by_node(&node).unwrap().is_some());
    assert!(s.authkey_by_node("deadbeef").unwrap().is_none());

    // expiry mutation + the migration backfill
    assert!(s.set_authkey_expiry(&node, Some(2000), false).unwrap());
    assert_eq!(s.authkey_by_node(&node).unwrap().unwrap().expires_at, Some(2000));
    assert!(!s.set_authkey_expiry("deadbeef", None, true).unwrap()); // no such key
    let filled = s.migrate_fill_expiry(9999).unwrap();
    assert!(filled <= 1); // already had an expiry; nothing (or itself) to fill

    assert!(s.delete_authkey_by_node(&node).unwrap());
    assert!(!s.delete_authkey_by_node(&node).unwrap()); // already gone
    assert!(s.authkeys_empty().unwrap());
}

// ── sqlite.rs: row / version-vector queries (via a real engine) ──────────────
#[test]
fn store_row_and_version_queries() {
    let dir = tempfile::tempdir().unwrap();
    let e = Engine::init(dir.path(), Identity::from_seed(&[7; 32])).unwrap();
    let wr = e.record_write("a.md", b"hello\n").unwrap().unwrap();
    e.record_write("b.md", b"world\n").unwrap();

    let s = &e.store;
    assert!(s.has_row(&wr.row.id).unwrap());
    assert!(!s.has_row("nonexistent-id").unwrap());
    assert!(s.row_count().unwrap() >= 2);
    assert!(!s.version_vector().unwrap().is_empty());
    assert!(s.next_lamport(0).unwrap() >= 1);
    assert!(s.next_seq(&e.site_id()).unwrap() >= 1);

    // The indexed path lookup the engine uses for echo suppression.
    let fid = s.file_id_for_path("a.md").unwrap();
    assert!(fid.is_some());
    assert_eq!(s.file_id_for_path("missing.md").unwrap(), None);

    // Paging matches the unpaged tail for a site.
    let site = e.site_id();
    let all = s.rows_after(&site, -1).unwrap();
    let page = s.rows_after_page(&site, -1, 1).unwrap();
    assert!(page.len() <= 1);
    assert!(all.len() >= page.len());

    // Cheap aggregates the status poll relies on (never load every row/file).
    // max_ts matches the max ts across all rows; live_file_count matches the
    // live (non-deleted) materialized file count.
    assert_eq!(s.max_ts().unwrap(), s.all_rows().unwrap().iter().map(|r| r.ts).max());
    assert_eq!(
        s.live_file_count().unwrap(),
        s.live_files().unwrap().iter().filter(|f| !f.deleted).count()
    );
    assert_eq!(s.live_file_count().unwrap(), 2);
    // A delete drops the live count (tombstones are not live).
    e.record_remove("a.md").unwrap();
    assert_eq!(s.live_file_count().unwrap(), 1);
}
