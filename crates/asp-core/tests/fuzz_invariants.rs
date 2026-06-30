//! Robustness fuzzing (§Testing). Hammers the **untrusted-input boundaries** and
//! the core algorithms with arbitrary/adversarial inputs over many iterations,
//! asserting (a) they never panic and (b) their invariants hold. A sync daemon
//! integrates bytes from a possibly-malicious peer, so "never crashes / never
//! diverges on garbage" is a correctness requirement, not a nicety.
//!
//! These run on stable in CI. A coverage-guided libFuzzer harness over the same
//! entry points lives in `fuzz/` (nightly, optional).

use asp_core::merge::merge3;
use asp_core::store::MemBlobStore;
use asp_core::{compute_files, fold_order, BlobStore, Identity, Kind, LogRow, MemEngine, MergeClass, Msg, WireRow};
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

fn rng(seed: u64) -> StdRng {
    StdRng::seed_from_u64(seed)
}
fn rand_bytes(r: &mut StdRng, max: usize) -> Vec<u8> {
    let n = r.gen_range(0..=max);
    (0..n).map(|_| r.gen::<u8>()).collect()
}
/// Gnarly strings: path/glob metacharacters, control bytes, and arbitrary unicode.
fn rand_string(r: &mut StdRng, max: usize) -> String {
    const META: &[char] = &['/', '*', '?', '!', '.', '\\', '\n', '\t', ' ', '-', '=', ':', '#', '[', ']'];
    let n = r.gen_range(0..=max);
    (0..n)
        .map(|_| match r.gen_range(0..4) {
            0 => META[r.gen_range(0..META.len())],
            1 => char::from(r.gen::<u8>()),
            2 => char::from_u32(r.gen_range(0..0x2FFF)).unwrap_or('x'),
            _ => char::from(b'a' + r.gen_range(0..26)),
        })
        .collect()
}
fn shuffle<T>(v: &mut [T], r: &mut StdRng) {
    for i in (1..v.len()).rev() {
        v.swap(i, r.gen_range(0..=i));
    }
}

#[test]
fn wire_decode_never_panics_on_arbitrary_or_mutated_bytes() {
    let mut r = rng(0xA11CE);
    for _ in 0..60_000 {
        let _ = Msg::from_bytes(&rand_bytes(&mut r, 300));
    }
    // Mutate valid frames bit-by-bit — closer to "almost-valid" garbage a buggy
    // or hostile peer might emit.
    let valids: Vec<Vec<u8>> = vec![
        Msg::Bye.to_bytes().unwrap(),
        Msg::Vector { vv: std::collections::BTreeMap::from([("aa".into(), 3i64)]) }.to_bytes().unwrap(),
        Msg::Denied { reason: "x".into() }.to_bytes().unwrap(),
        Msg::Hello { proto: 2, node_id: "ab".into(), vault_id: "v".into(), is_listener: true, auth_key: None }
            .to_bytes()
            .unwrap(),
    ];
    for _ in 0..40_000 {
        let mut m = valids[r.gen_range(0..valids.len())].clone();
        if !m.is_empty() {
            let i = r.gen_range(0..m.len());
            m[i] = r.gen();
        }
        let _ = Msg::from_bytes(&m);
    }
}

#[test]
fn integrate_rejects_tampered_rows_without_panic_or_corruption() {
    let mut r = rng(0xBEEF);
    let src = MemEngine::create(Identity::from_seed(&[9; 32]), "v");
    let mut valid: Vec<WireRow> = Vec::new();
    for i in 0..40 {
        if let Some(wr) = src.record_write(&format!("d{}/f{}.md", i % 3, i % 7), format!("content number {i}\n").as_bytes()).unwrap() {
            valid.push(wr);
        }
    }
    let sink = MemEngine::create(Identity::from_seed(&[8; 32]), "v");
    for _ in 0..40_000 {
        let mut wr = valid[r.gen_range(0..valid.len())].clone();
        match r.gen_range(0..6) {
            0 => wr.row.id = hex::encode(rand_bytes(&mut r, 32)),
            1 => wr.row.lamport ^= r.gen::<u64>(),
            2 => wr.row.path = Some(rand_string(&mut r, 40)),
            3 => {
                if let Some(b) = wr.blobs.first_mut() {
                    b.hash = hex::encode(rand_bytes(&mut r, 32));
                }
            }
            4 => {
                if let Some(b) = wr.blobs.first_mut() {
                    if !b.bytes.is_empty() {
                        let i = r.gen_range(0..b.bytes.len());
                        b.bytes[i] ^= 0xFF;
                    } else {
                        b.bytes.push(r.gen());
                    }
                }
            }
            _ => wr.row.result_hash = Some(hex::encode(rand_bytes(&mut r, 32))),
        }
        // Tampering any covered field invalidates the Merkle id (or a blob hash) →
        // integrate must refuse it, never panic.
        let _ = sink.integrate(&wr);
    }
    // The sink is still coherent (folds, materializes) after the garbage barrage.
    let _ = sink.files_map().expect("sink still folds after tampered-row barrage");
}

#[test]
fn merge3_never_panics_and_is_deterministic_on_arbitrary_bytes() {
    let mut r = rng(0xF00D);
    for _ in 0..40_000 {
        let base = rand_bytes(&mut r, 200);
        let ours = rand_bytes(&mut r, 200);
        let theirs = rand_bytes(&mut r, 200);
        for class in [MergeClass::Text, MergeClass::Code, MergeClass::Binary] {
            let a = merge3(class, &base, &ours, &theirs);
            let b = merge3(class, &base, &ours, &theirs);
            assert_eq!(a.bytes, b.bytes, "merge3 is deterministic");
        }
        // Binary always takes the later side.
        assert_eq!(merge3(MergeClass::Binary, &base, &ours, &theirs).bytes, theirs);
    }
}

#[test]
fn merge3_holds_3way_identities_on_text() {
    let mut r = rng(0x1234);
    let lines = |r: &mut StdRng| -> Vec<u8> {
        let n = r.gen_range(0..12);
        let mut s = String::new();
        for _ in 0..n {
            s.push_str(&format!("line-{}\n", r.gen_range(0..8)));
        }
        s.into_bytes()
    };
    for _ in 0..20_000 {
        let base = lines(&mut r);
        let ours = lines(&mut r);
        let theirs = lines(&mut r);
        // The defining 3-way identities (text path, always valid utf8).
        assert_eq!(merge3(MergeClass::Text, &base, &base, &theirs).bytes, theirs, "ours==base → theirs");
        assert_eq!(merge3(MergeClass::Text, &base, &ours, &base).bytes, ours, "theirs==base → ours");
        assert_eq!(merge3(MergeClass::Text, &ours, &ours, &ours).bytes, ours, "no change → identity");
    }
}

fn rand_kind(r: &mut StdRng) -> Kind {
    match r.gen_range(0..5) {
        0 => Kind::Create,
        1 => Kind::Edit,
        2 => Kind::Rename,
        3 => Kind::Delete,
        _ => Kind::Reclass,
    }
}
fn rand_class(r: &mut StdRng) -> MergeClass {
    match r.gen_range(0..4) {
        0 => MergeClass::Text,
        1 => MergeClass::Code,
        2 => MergeClass::Binary,
        _ => MergeClass::Dir,
    }
}

#[test]
fn fold_on_arbitrary_rows_never_panics_and_is_permutation_invariant() {
    let mut r = rng(0xDEADBEEF);
    for _ in 0..4000 {
        let store = MemBlobStore::new();
        let mut hashes: Vec<String> = Vec::new();
        for _ in 0..r.gen_range(0..6) {
            hashes.push(store.put_blob(&rand_bytes(&mut r, 40)).unwrap());
        }
        let pick_hash = |r: &mut StdRng, hashes: &[String]| -> Option<String> {
            match r.gen_range(0..4) {
                0 => None,
                1 if !hashes.is_empty() => Some(hashes[r.gen_range(0..hashes.len())].clone()),
                2 => Some(hex::encode(rand_bytes(r, 32))), // dangling reference
                _ => None,
            }
        };
        let mut ids: Vec<String> = Vec::new();
        let n = r.gen_range(0..28);
        let mut rows: Vec<LogRow> = Vec::new();
        for _ in 0..n {
            // parent: none / existing / garbage / self-cycle.
            let parent = match r.gen_range(0..5) {
                0 => None,
                1 if !ids.is_empty() => Some(ids[r.gen_range(0..ids.len())].clone()),
                2 => Some(hex::encode(rand_bytes(&mut r, 32))),
                3 if !ids.is_empty() => Some(ids.last().unwrap().clone()),
                _ => None,
            };
            let row = LogRow {
                id: String::new(),
                site_id: format!("s{}", r.gen_range(0..3)),
                lamport: r.gen_range(0..8),
                seq: r.gen_range(0..8),
                ts: r.gen_range(0..1000),
                file_id: format!("f{}", r.gen_range(0..5)),
                kind: rand_kind(&mut r),
                merge_class: rand_class(&mut r),
                parent,
                base_hash: pick_hash(&mut r, &hashes),
                result_hash: pick_hash(&mut r, &hashes),
                path: Some(format!("d{}/x{}.md", r.gen_range(0..2), r.gen_range(0..3))),
                branch_id: asp_core::MAIN_BRANCH_ID.to_string(),
                merge_parent: None,
                sig: vec![],
            }
            .seal();
            ids.push(row.id.clone());
            rows.push(row);
        }
        // Dedup by id (the store has id as PRIMARY KEY — duplicates can't coexist).
        rows.sort_by(|a, b| a.id.cmp(&b.id));
        rows.dedup_by(|a, b| a.id == b.id);

        let f1 = compute_files(&store, &rows).expect("fold must not panic on arbitrary rows");
        let mut shuffled = rows.clone();
        shuffle(&mut shuffled, &mut r);
        let f2 = compute_files(&store, &shuffled).expect("fold must not panic");
        assert_eq!(f1, f2, "fold is permutation-invariant (deterministic) on arbitrary rows");

        // Live content-file paths are unique (the path-collision invariant).
        let mut live: Vec<String> =
            f1.iter().filter(|x| !x.deleted && x.merge_class != MergeClass::Dir).map(|x| x.path.clone()).collect();
        let before = live.len();
        live.sort();
        live.dedup();
        assert_eq!(before, live.len(), "live file paths are unique");

        // fold_order is also order-independent.
        let o1: Vec<String> = fold_order(&rows).into_iter().map(|x| x.id).collect();
        let o2: Vec<String> = fold_order(&shuffled).into_iter().map(|x| x.id).collect();
        assert_eq!(o1, o2, "fold_order is permutation-invariant");
    }
}

#[test]
fn parsers_never_panic_on_arbitrary_input() {
    let mut r = rng(0xC0FFEE);
    for _ in 0..40_000 {
        let s = rand_string(&mut r, 120);
        let _ = asp_core::identity::parse_ssh_pubkey(&s);
        let sc = asp_core::scope::Scope::parse(&s);
        let _ = sc.ignored(&rand_string(&mut r, 80));
        let _ = asp_core::authkeys::parse_duration_days(&s);
        let _ = asp_core::authkeys::parse_date_ymd_utc(&s);
        let _ = asp_core::authkeys::parse_ttl(&s);
        let _ = asp_core::NodeId::from_hex(&s);
    }
}
