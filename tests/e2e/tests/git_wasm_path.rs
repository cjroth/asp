//! The **wasm browser-clone path's Rust half**, driven end-to-end from the recorded
//! smart-HTTP protocol-v2 wire fixtures (git-bridge §7.3, §10 "wasm path").
//!
//! This is the exact chain `asp_wasm::WasmEngine::git_clone` runs *after* the JS
//! `fetch()` transport hands back bytes: parse the info/refs advertisement, parse the
//! ls-refs response (→ HEAD symref + tip), side-band-demux the fetch response's
//! packfile, decode it, plan the import, synthesize deterministic genesis, and fold
//! it into a pristine `MemEngine` (the wasm engine). Fully hermetic — no system git,
//! no network — so it always runs, unlike the pack-building fixture tests.
//!
//! The fixtures were recorded from a real `git http-backend` (see
//! `src/bin/record_fixtures.rs`) against the `linear_basic` repo, so this proves the
//! browser code path against genuine GitHub-shaped wire bytes.

use std::path::PathBuf;

use asp_core::gitgenesis::{git_vault_id, synthesize_genesis, DbBlobSource};
use asp_core::gitimport::{no_base_lookup, plan_import, GitObjectDb, ImportOptions};
use asp_core::gitwire::{parse_capability_advertisement, parse_ls_refs_response, FetchResponseParser};
use asp_core::identity::Identity;
use asp_core::store::{BlobStore, MemBlobStore};
use asp_core::wire::{WireBlob, WireRow};
use asp_core::{LogRow, MemEngine, SessionVault};

fn fixture(name: &str) -> Vec<u8> {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures").join(name);
    std::fs::read(&p).unwrap_or_else(|e| panic!("read fixture {}: {e}", p.display()))
}

/// Bundle each row with its content blobs (mirrors `asp_wasm::to_wires`).
fn to_wires(rows: &[LogRow], store: &MemBlobStore) -> Vec<WireRow> {
    rows.iter()
        .map(|r| {
            let mut blobs: Vec<WireBlob> = Vec::new();
            for h in [r.base_hash.clone(), r.result_hash.clone()].into_iter().flatten() {
                if let Some(bytes) = store.get_blob(&h).ok().flatten() {
                    if !blobs.iter().any(|b| b.hash == h) {
                        blobs.push(WireBlob { hash: h, bytes });
                    }
                }
            }
            WireRow { row: r.clone(), blobs }
        })
        .collect()
}

#[test]
fn wasm_clone_from_recorded_wire_fixtures() {
    // 1. info/refs advertisement → protocol-v2 capabilities (must be sha1).
    let info = fixture("info_refs_v2.bin");
    let caps = parse_capability_advertisement(&info).expect("parse advertisement");
    assert!(caps.supports("ls-refs") && caps.supports("fetch"));
    caps.object_format().expect("sha1 object format accepted");

    // 2. ls-refs → HEAD symref target + tip (what `resolve_head` extracts in wasm).
    let ls = fixture("ls_refs_v2.bin");
    let refs = parse_ls_refs_response(&ls).expect("parse ls-refs");
    let head = refs.iter().find(|r| r.name == "HEAD").expect("HEAD advertised");
    assert_eq!(head.symref_target.as_deref(), Some("refs/heads/main"));
    let tip = head.oid.clone();
    assert_eq!(tip, "89d2010db2188ca6e11e8eaf7a844e7eea72f869");

    // 3. fetch response → side-band-demuxed packfile.
    let fetch = fixture("fetch_v2.bin");
    let resp = FetchResponseParser::parse(&fetch).expect("parse fetch response");
    assert!(resp.saw_packfile, "fetch response carried a packfile section");
    assert_eq!(&resp.pack[..4], b"PACK", "reassembled band-1 pack bytes");

    // 4/5. decode → plan → deterministic genesis (full clone: no external bases).
    let db = GitObjectDb::from_pack(&resp.pack, no_base_lookup).expect("decode pack");
    let plan = plan_import(&db, &tip, &ImportOptions::default()).expect("plan import");
    assert_eq!(plan.tip_sha, tip);
    let scratch = MemBlobStore::new();
    let g = synthesize_genesis(&plan, &DbBlobSource::new(&db), &scratch).expect("genesis");
    assert_eq!(g.vault_id, git_vault_id(&plan.root_sha), "vault id is repo-derived");

    // 6. fold into a pristine MemEngine (the wasm engine) and check the tip tree.
    let eng = MemEngine::create(Identity::from_seed(&[5u8; 32]), "");
    eng.adopt_vault_id(&g.vault_id).unwrap();
    eng.integrate_many(&to_wires(&g.rows, &scratch)).unwrap();
    let files = eng.files_map().unwrap();

    // linear_basic's tip tree: a.txt renamed → a2.txt (final 3-line content),
    // dir/b.txt deleted, dir/c.txt added — plus the clone-seeded .aspignore.
    assert_eq!(files.get("a2.txt").map(|v| v.as_slice()), Some(&b"alpha\nalpha2\nalpha3\n"[..]));
    assert_eq!(files.get("dir/c.txt").map(|v| v.as_slice()), Some(&b"charlie\n"[..]));
    assert!(!files.contains_key("a.txt"), "a.txt was renamed away");
    assert!(!files.contains_key("dir/b.txt"), "dir/b.txt was deleted");
    assert!(files.contains_key(".aspignore"), "clone seeds .aspignore");

    // Determinism: an independent decode+genesis authors byte-identical rows (so two
    // browsers cloning the same URL converge over ordinary ASP sync, git-bridge §3.2).
    let db2 = GitObjectDb::from_pack(&resp.pack, no_base_lookup).unwrap();
    let plan2 = plan_import(&db2, &tip, &ImportOptions::default()).unwrap();
    let scratch2 = MemBlobStore::new();
    let g2 = synthesize_genesis(&plan2, &DbBlobSource::new(&db2), &scratch2).unwrap();
    assert_eq!(g.rows, g2.rows, "independent clones author identical rows");
    assert_eq!(g.vault_id, g2.vault_id);
}
