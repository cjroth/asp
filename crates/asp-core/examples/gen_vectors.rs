//! Generate cross-surface conformance vectors from the **native** engine
//! (§Testing: cross-surface interop). The wasm/TS SDK asserts byte-identity
//! against these — if the wasm fold/merge/identity ever skews from native, the
//! SDK conformance test fails. Regenerate with:
//!   cargo run -p asp-core --example gen_vectors > sdks/typescript/test-vectors.json

use asp_core::log::{Kind, LogRow, MergeClass};
use asp_core::merge::merge3;
use asp_core::store::{BlobStore, MemBlobStore};
use asp_core::{compute_files, oid, Identity};
use std::collections::BTreeMap;

#[allow(clippy::too_many_arguments)]
fn row(
    site: &str,
    lamport: u64,
    seq: u64,
    file_id: &str,
    kind: Kind,
    parent: Option<&str>,
    base: Option<&str>,
    result: Option<&str>,
    path: Option<&str>,
) -> LogRow {
    LogRow {
        id: String::new(),
        site_id: site.into(),
        lamport,
        seq,
        ts: 0,
        file_id: file_id.into(),
        kind,
        merge_class: MergeClass::Text,
        parent: parent.map(String::from),
        base_hash: base.map(String::from),
        result_hash: result.map(String::from),
        path: path.map(String::from),
        branch_id: asp_core::MAIN_BRANCH_ID.to_string(),
        merge_parent: None,
        sig: vec![],
    }
    .seal()
}

fn main() {
    let seed = [7u8; 32];
    let id = Identity::from_seed(&seed);

    // A fold scenario: a base + two concurrent edits to different lines (both
    // survive), folded deterministically.
    let store = MemBlobStore::new();
    let base = b"l1\nl2\nl3\n";
    let hb = store.put_blob(base).unwrap();
    let ha = store.put_blob(b"A1\nl2\nl3\n").unwrap();
    let hc = store.put_blob(b"l1\nl2\nC3\n").unwrap();

    let create = row("aa", 1, 0, "f1", Kind::Create, None, None, Some(&hb), Some("doc.md"));
    let edit_a = row("aa", 2, 1, "f1", Kind::Edit, Some(&create.id), Some(&hb), Some(&ha), None);
    let edit_c = row("bb", 2, 0, "f1", Kind::Edit, Some(&create.id), Some(&hb), Some(&hc), None);
    let rows = vec![create, edit_a, edit_c];

    let files = compute_files(&store, &rows).unwrap();
    let mut expected: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for f in &files {
        if f.deleted {
            continue;
        }
        if let Some(h) = &f.result_hash {
            expected.insert(f.path.clone(), store.get_blob(h).unwrap().unwrap());
        }
    }

    let blobs: BTreeMap<String, Vec<u8>> = [&hb, &ha, &hc]
        .iter()
        .map(|h| ((*h).clone(), store.get_blob(h).unwrap().unwrap()))
        .collect();

    // Merge vectors: text same-region resolves clean; code surfaces markers.
    let text_merge = merge3(MergeClass::Text, b"x\n", b"ours\n", b"theirs\n").bytes;
    let code_merge = merge3(MergeClass::Code, b"x\n", b"ours\n", b"theirs\n").bytes;

    let vectors = serde_json::json!({
        "seed_hex": hex::encode(seed),
        "node_id": id.node_id().to_hex(),
        "ssh_pubkey": id.to_ssh_string(),
        "content_hash_hello": oid::content_hash(b"hello\n"),
        "fold": {
            "rows": rows,
            "blobs": blobs,
            "expected_files": expected,
        },
        "merge": {
            "text_same_region": text_merge,
            "code_same_region": code_merge,
        },
    });
    println!("{}", serde_json::to_string_pretty(&vectors).unwrap());
}
