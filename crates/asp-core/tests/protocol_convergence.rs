//! Protocol-level convergence fuzz (the layer the reconnect bug lived in). Unlike
//! `property_convergence` — which delivers *every* row to *every* node (full mesh)
//! and so only exercises the fold — this drives the **real sans-IO `Session`**:
//! the mutual-auth handshake plus **version-vector catch-up**, which decides which
//! rows to send. Random interleavings of "author locally" and "sync a random pair"
//! model offline edits + reconnects; a final all-pairs reconciliation must
//! converge. A catch-up that drops rows (e.g. a `(site_id, seq)` collision, the
//! reported bug) shows up here as divergence.

use asp_core::session::Step;
use asp_core::{AdmitCtx, Identity, MemEngine, Msg, Role, Session, SessionVault};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

fn ctx() -> AdmitCtx {
    AdmitCtx { no_tofu: false, auth_key_ok: false, auth_key_configured: false, default_ttl_days: 90, now_unix: 1_700_000_000 }
}
fn sends(steps: Vec<Step>) -> Vec<Msg> {
    steps.into_iter().filter_map(|s| if let Step::Send(m) = s { Some(m) } else { None }).collect()
}

/// Drive a full handshake + bidirectional version-vector catch-up between two
/// engines over an in-process message pump (no sockets — the same `Session`).
fn sync_pair(listener: &MemEngine, connector: &MemEngine) {
    // Each side's verified peer is the other's key (what iroh would authenticate).
    let mut l = Session::new(Role::Listener, listener, ctx(), SessionVault::node_id(connector), Vec::new());
    let mut c = Session::new(Role::Connector, connector, ctx(), SessionVault::node_id(listener), Vec::new());
    let mut to_l: Vec<Msg> = sends(c.start());
    let mut to_c: Vec<Msg> = sends(l.start());
    for _ in 0..64 {
        let mut n_to_l = Vec::new();
        let mut n_to_c = Vec::new();
        for m in to_l.drain(..) {
            n_to_c.extend(sends(l.on_msg(listener, m).expect("listener step")));
        }
        for m in to_c.drain(..) {
            n_to_l.extend(sends(c.on_msg(connector, m).expect("connector step")));
        }
        to_l = n_to_l;
        to_c = n_to_c;
        if to_l.is_empty() && to_c.is_empty() {
            break;
        }
    }
}

fn build(ids: &[Identity]) -> Vec<MemEngine> {
    let engines: Vec<MemEngine> = ids.iter().map(|id| MemEngine::create(id.clone(), "V")).collect();
    // Pre-authorize every distinct device key on every node so admission always
    // passes (we're fuzzing catch-up, not auth).
    for e in &engines {
        for id in ids {
            e.authorize(&id.to_ssh_string(), None, true, "test").unwrap();
        }
    }
    engines
}

fn run(seed: u64, ids: &[Identity], steps: usize) {
    let mut rng = StdRng::seed_from_u64(seed);
    let engines = build(ids);
    let k = engines.len();
    let paths = ["a.md", "b.md", "dir/c.md", "doc.txt"];

    for _ in 0..steps {
        match rng.gen_range(0..3u8) {
            0 | 1 => {
                // Author locally (possibly while "offline").
                let e = rng.gen_range(0..k);
                let p = paths[rng.gen_range(0..paths.len())];
                let body = format!("l1\nval-{}\nl3\n", rng.gen_range(0..6));
                let _ = engines[e].record_write(p, body.as_bytes());
            }
            _ => {
                // Reconnect a random pair and run real catch-up.
                let a = rng.gen_range(0..k);
                let b = (a + 1 + rng.gen_range(0..k - 1)) % k;
                sync_pair(&engines[a], &engines[b]);
            }
        }
    }

    // Final reconciliation: every ordered pair, a couple rounds, so a connected
    // mesh fully propagates.
    for _ in 0..2 {
        for a in 0..k {
            for b in 0..k {
                if a != b {
                    sync_pair(&engines[a], &engines[b]);
                }
            }
        }
    }

    let base = engines[0].files_map().expect("files");
    for (i, e) in engines.iter().enumerate().skip(1) {
        assert_eq!(e.files_map().expect("files"), base, "seed {seed}: node {i} did not converge via catch-up");
    }
}

#[test]
fn catchup_converges_under_random_offline_reconnect() {
    for seed in 0..30 {
        let ids: Vec<Identity> = (0..3).map(|i| Identity::from_seed(&[(i as u8) + 1; 32])).collect();
        run(seed, &ids, 18);
    }
}

#[test]
fn catchup_converges_with_shared_device_identity() {
    // The reported bug: replicas on ONE device share the device key. With distinct
    // per-vault `site_id`s their concurrent edits no longer collide on
    // `(site_id, seq)`, so catch-up still exchanges everything and they converge.
    for seed in 0..20 {
        let ids: Vec<Identity> = (0..3).map(|_| Identity::from_seed(&[7u8; 32])).collect(); // SAME device key
        run(seed, &ids, 18);
    }
}
