//! WebAssembly bindings for `asp-core` (§Implementation: the wasm/TypeScript SDK).
//! This is **not** a reimplementation — it is the *real* full engine compiled to
//! wasm. The high-level [`WasmEngine`] drives the same `MemEngine` (capture +
//! `compute_files` fold + `merge3`) and the same sans-IO `Session` (handshake +
//! version-vector catch-up) as the native daemon, so a browser/Obsidian node
//! computes byte-identical state. The low-level functions back the cross-surface
//! conformance vectors (wasm output == native output).

use asp_core::{
    compute_files, identity::ssh_pubkey_string, merge::merge3, oid, store::MemBlobStore, BlobStore,
    FileRow, Identity, LogRow, MemEngine, MergeClass, SessionVault, WireRow,
};
use std::collections::BTreeMap;
use wasm_bindgen::prelude::*;

fn ident(seed: &[u8]) -> Identity {
    let mut s = [0u8; 32];
    let n = seed.len().min(32);
    s[..n].copy_from_slice(&seed[..n]);
    Identity::from_seed(&s)
}

// ---------------- low-level conformance surface ----------------

/// The node's ed25519 public-key hex for a 32-byte seed (must match native).
#[wasm_bindgen]
pub fn node_id_hex(seed: &[u8]) -> String {
    ident(seed).node_id().to_hex()
}

/// OpenSSH public-key line for a seed (must match native `asp key`).
#[wasm_bindgen]
pub fn ssh_pubkey(seed: &[u8], comment: &str) -> String {
    ssh_pubkey_string(&ident(seed).node_id(), comment)
}

/// Content hash of bytes (the `blobs` key + Merkle-id building block).
#[wasm_bindgen]
pub fn content_hash(bytes: &[u8]) -> String {
    oid::content_hash(bytes)
}

/// The Merkle id of a row given as JSON (proves identical row hashing).
#[wasm_bindgen]
pub fn merkle_id_of(row_json: &str) -> Result<String, JsError> {
    let row: LogRow = serde_json::from_str(row_json).map_err(to_err)?;
    Ok(row.seal().id)
}

/// 3-way merge under a `merge_class` (`text`|`code`|`binary`) — byte-identical to
/// native (the headline merge gate, in wasm).
#[wasm_bindgen]
pub fn merge3_bytes(class: &str, base: &[u8], ours: &[u8], theirs: &[u8]) -> Vec<u8> {
    let mc = MergeClass::parse(class).unwrap_or(MergeClass::Text);
    merge3(mc, base, ours, theirs).bytes
}

/// Deterministic fold conformance: given the log rows (JSON array) and the blobs
/// they reference (`{hash: [u8]}`), return the materialized files map as JSON
/// `{path: [u8]}`. This must equal the native fold over the same inputs.
#[wasm_bindgen]
pub fn fold_files(rows_json: &str, blobs_json: &str) -> Result<String, JsError> {
    let rows: Vec<LogRow> = serde_json::from_str(rows_json).map_err(to_err)?;
    let blobs: BTreeMap<String, Vec<u8>> = serde_json::from_str(blobs_json).map_err(to_err)?;
    let store = MemBlobStore::new();
    for bytes in blobs.values() {
        store.put_blob(bytes).map_err(to_err)?;
    }
    let files = compute_files(&store, &rows).map_err(to_err)?;
    let mut out: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    for f in files {
        if f.deleted {
            continue;
        }
        if let Some(h) = f.result_hash {
            out.insert(f.path, store.get_blob(&h).map_err(to_err)?.unwrap_or_default());
        }
    }
    serde_json::to_string(&out).map_err(to_err)
}

// ---------------- high-level thin-node engine ----------------

#[wasm_bindgen]
pub struct WasmEngine {
    eng: std::rc::Rc<MemEngine>,
    // When a live connection is open, freshly-authored rows are handed to it here
    // so they push to the peer immediately (None when there's no live link).
    live_tx: std::cell::RefCell<Option<futures_channel::mpsc::UnboundedSender<WireRow>>>,
}

#[wasm_bindgen]
impl WasmEngine {
    /// Create a thin node authoring as `seed`. An empty `vault_id` adopts the
    /// peer's vault on the first connect (clone).
    #[wasm_bindgen(constructor)]
    pub fn new(seed: &[u8], vault_id: &str) -> WasmEngine {
        #[cfg(target_arch = "wasm32")]
        console_error_panic_hook::set_once();
        WasmEngine {
            eng: std::rc::Rc::new(MemEngine::create(ident(seed), vault_id)),
            live_tx: std::cell::RefCell::new(None),
        }
    }

    /// Hand newly-authored rows to an open live connection (a no-op otherwise).
    fn push_live(&self, rows: Vec<WireRow>) {
        if let Some(tx) = self.live_tx.borrow().as_ref() {
            for r in rows {
                let _ = tx.unbounded_send(r);
            }
        }
    }

    pub fn node_id(&self) -> String {
        self.eng.site_id()
    }

    pub fn node_ssh(&self) -> String {
        ssh_pubkey_string(&SessionVault::node_id(&*self.eng), "asp")
    }

    pub fn vault_id(&self) -> String {
        SessionVault::vault_id(&*self.eng)
    }

    pub fn row_count(&self) -> usize {
        self.eng.row_count()
    }

    /// Author a create/edit for `path`.
    pub fn record_write(&self, path: &str, content: &[u8]) -> Result<(), JsError> {
        if let Some(wr) = self.eng.record_write(path, content).map_err(to_err)? {
            self.push_live(vec![wr]);
        }
        Ok(())
    }

    pub fn record_remove(&self, path: &str) -> Result<(), JsError> {
        if let Some(wr) = self.eng.record_remove(path).map_err(to_err)? {
            self.push_live(vec![wr]);
        }
        Ok(())
    }

    pub fn record_rename(&self, from: &str, to: &str) -> Result<(), JsError> {
        if let Some(wr) = self.eng.record_rename(from, to).map_err(to_err)? {
            self.push_live(vec![wr]);
        }
        Ok(())
    }

    /// Whole-set commit from the host's current vault contents (`{path: [u8]}`).
    pub fn commit_files(&self, files_json: &str) -> Result<(), JsError> {
        let files: BTreeMap<String, Vec<u8>> = serde_json::from_str(files_json).map_err(to_err)?;
        let rows = self.eng.commit_files(&files).map_err(to_err)?;
        self.push_live(rows);
        Ok(())
    }

    /// Stage a batch of host files (create/edit, no deletes), folding ONCE
    /// (`{path: [u8]}`). The startup reconcile uses this instead of one
    /// record_write per file — per-file re-folding is O(n²) over a large vault.
    pub fn write_files(&self, files_json: &str) -> Result<(), JsError> {
        let files: BTreeMap<String, Vec<u8>> = serde_json::from_str(files_json).map_err(to_err)?;
        let rows = self.eng.record_writes(&files).map_err(to_err)?;
        self.push_live(rows);
        Ok(())
    }

    /// Author deletes for a JSON array of paths, folding ONCE — the startup
    /// reconcile uses this to capture files deleted while the host app was
    /// closed (no delete events fire for those; without it the peer's copy
    /// resurrects them on the next materialize).
    pub fn remove_files(&self, paths_json: &str) -> Result<(), JsError> {
        let paths: Vec<String> = serde_json::from_str(paths_json).map_err(to_err)?;
        let rows = self.eng.record_removes(&paths).map_err(to_err)?;
        self.push_live(rows);
        Ok(())
    }

    /// Serialize the full engine state as compact msgpack bytes (rows + each
    /// blob once) — the persistable form for thin clients. The JSON wire dump
    /// (`rows_after({})`) duplicates blobs per row and inflates every byte to
    /// ~4 chars, which OOMs a mobile WebView on a large vault.
    pub fn dump_state(&self) -> Result<Vec<u8>, JsError> {
        self.eng.export_state().map_err(to_err)
    }

    /// Restore a `dump_state` snapshot (validates row ids + blob hashes).
    /// Returns the number of rows newly integrated.
    pub fn load_state(&self, bytes: &[u8]) -> Result<usize, JsError> {
        self.eng.import_state(bytes).map_err(to_err)
    }

    /// The materialized working tree as JSON `{path: [u8]}`.
    pub fn files_json(&self) -> Result<String, JsError> {
        let m = self.eng.files_map().map_err(to_err)?;
        serde_json::to_string(&m).map_err(to_err)
    }

    pub fn read_file(&self, path: &str) -> Result<Option<Vec<u8>>, JsError> {
        self.eng.read_file(path).map_err(to_err)
    }

    // ---------------- branches (§2, §7) ----------------

    /// The checked-out branch id (HEAD).
    pub fn current_branch(&self) -> String {
        self.eng.current_branch()
    }

    /// All live branches as JSON: `[{branch_id, name, parent, created_lamport}]`.
    pub fn branches_json(&self) -> Result<String, JsError> {
        #[derive(serde::Serialize)]
        struct B<'a> {
            branch_id: &'a str,
            name: &'a str,
            parent: Option<&'a str>,
            created_lamport: u64,
        }
        let bs = self.eng.branches();
        let out: Vec<B> = bs
            .iter()
            .map(|b| B { branch_id: &b.branch_id, name: &b.name, parent: b.parent.as_deref(), created_lamport: b.created_lamport })
            .collect();
        serde_json::to_string(&out).map_err(to_err)
    }

    /// Create a branch off `parent` (its current version vector becomes the fork
    /// point). Returns the new branch id. Does not switch HEAD.
    pub fn create_branch(&self, name: &str, parent: &str) -> Result<String, JsError> {
        // Fork at the parent branch's current visible vv — the common "branch from
        // here" case. (Edit-in-the-past forks at a timestamp via fork_at.)
        let vv = self.fork_vv_now(parent).map_err(to_err)?;
        self.eng.create_branch(name, parent, vv).map_err(to_err)
    }

    /// Edit-in-the-past ⇒ branch (§2.5): fork HEAD at wall-clock `t` and switch to
    /// the new branch. Returns its id.
    pub fn fork_at(&self, name: &str, t: f64) -> Result<String, JsError> {
        // `NaN as i64` saturates to 0 — a silent fork "before the beginning" that
        // captures no rows. Reject non-finite timestamps at the JS boundary instead.
        if !t.is_finite() {
            return Err(JsError::new("fork timestamp must be a finite number"));
        }
        self.eng.fork_from_time(name, t as i64).map_err(to_err)
    }

    /// Switch HEAD to `branch_id` and re-materialize its scoped state.
    pub fn checkout(&self, branch_id: &str) -> Result<(), JsError> {
        self.eng.checkout(branch_id).map_err(to_err)
    }

    /// Soft-delete a branch (main cannot be deleted).
    pub fn delete_branch(&self, branch_id: &str) -> Result<(), JsError> {
        self.eng.delete_branch(branch_id).map_err(to_err)
    }

    /// The branch/commit DAG (GitHub-network-style) as JSON `{nodes, branches, tags}`,
    /// bounded to `cap` commits per lane.
    pub fn graph_json(&self, cap: u32) -> Result<String, JsError> {
        serde_json::to_string(&self.eng.graph(cap as usize)).map_err(to_err)
    }

    // ---------------- tags ----------------

    /// Live tags as JSON: `[{tag_id, name, at_ts, branch_id}]`.
    pub fn tags_json(&self) -> Result<String, JsError> {
        #[derive(serde::Serialize)]
        struct T<'a> {
            tag_id: &'a str,
            name: &'a str,
            at_ts: i64,
            branch_id: &'a str,
        }
        let ts = self.eng.tags();
        let out: Vec<T> = ts
            .iter()
            .map(|t| T { tag_id: &t.tag_id, name: &t.name, at_ts: t.at_ts, branch_id: &t.branch_id })
            .collect();
        serde_json::to_string(&out).map_err(to_err)
    }

    /// Tag the point at wall-clock `at_ts` (unix seconds) on the current branch.
    pub fn create_tag(&self, name: &str, at_ts: f64) -> Result<String, JsError> {
        if !at_ts.is_finite() {
            return Err(JsError::new("tag timestamp must be a finite number"));
        }
        self.eng.create_tag(name, at_ts as i64).map_err(to_err)
    }

    /// Soft-delete a tag.
    pub fn delete_tag(&self, tag_id: &str) -> Result<(), JsError> {
        self.eng.delete_tag(tag_id).map_err(to_err)
    }

    // ---------------- history + time travel (PITR) ----------------

    /// The append-only history as JSON `[{id, ts, lamport, kind, path, branch_id}]`
    /// — drives the timeline. (Web parity with the native `history` command.)
    pub fn history_json(&self) -> Result<String, JsError> {
        #[derive(serde::Serialize)]
        struct H {
            id: String,
            ts: i64,
            lamport: u64,
            kind: String,
            path: String,
            branch_id: String,
        }
        let out: Vec<H> = self
            .eng
            .history()
            .into_iter()
            .map(|(id, ts, lamport, kind, path, branch_id)| H { id, ts, lamport, kind, path, branch_id })
            .collect();
        serde_json::to_string(&out).map_err(to_err)
    }

    /// Content of `path` as of wall-clock `t` (unix seconds) as JSON `{exists, content}`.
    pub fn file_at_json(&self, path: &str, t: f64) -> Result<String, JsError> {
        if !t.is_finite() {
            return Err(JsError::new("time must be a finite number"));
        }
        #[derive(serde::Serialize)]
        struct FA {
            exists: bool,
            content: String,
        }
        let fa = match self.eng.file_at(path, t as i64).map_err(to_err)? {
            Some(bytes) => FA { exists: true, content: String::from_utf8_lossy(&bytes).into_owned() },
            None => FA { exists: false, content: String::new() },
        };
        serde_json::to_string(&fa).map_err(to_err)
    }

    /// Restore `path` to its content as of `t` (records it as a new edit).
    pub fn restore_file_at(&self, path: &str, t: f64) -> Result<(), JsError> {
        if !t.is_finite() {
            return Err(JsError::new("time must be a finite number"));
        }
        self.eng.restore_file_at(path, t as i64).map_err(to_err)?;
        Ok(())
    }

    /// The version vector visible on `branch` right now (the fork point a child
    /// branch captures). Internal helper for `create_branch`.
    fn fork_vv_now(&self, branch: &str) -> asp_core::AspResult<asp_core::VersionVector> {
        // Fork "from now" = the parent's full visible vv (every visible row is an
        // ancestor). fork_from_time with t=i64::MAX yields exactly this set, but we
        // only need the vv, not a checkout, so compute it from the engine's rows.
        Ok(self.eng.visible_version_vector(branch))
    }

    /// This node's version vector as JSON `{site_id: max_seq}` — the catch-up
    /// cursor a peer hands us so we can compute exactly what it lacks.
    pub fn version_vector(&self) -> Result<String, JsError> {
        let vv = SessionVault::version_vector(&*self.eng).map_err(to_err)?;
        serde_json::to_string(&vv).map_err(to_err)
    }

    /// Given a *peer's* version vector (JSON `{site_id: seq}`), return the wire
    /// rows that peer is missing as a JSON array — the exact anti-entropy /
    /// catch-up payload. The same op drives live push, gossip forwarding, and
    /// reconnect (offline → reconnect → version-vector catch-up).
    pub fn rows_after(&self, peer_vv_json: &str) -> Result<String, JsError> {
        let peer_vv: std::collections::BTreeMap<String, i64> =
            serde_json::from_str(peer_vv_json).map_err(to_err)?;
        let mine = SessionVault::version_vector(&*self.eng).map_err(to_err)?;
        let mut out: Vec<WireRow> = Vec::new();
        for site in mine.keys() {
            let after = peer_vv.get(site).copied().unwrap_or(-1);
            let mut rows = SessionVault::rows_after_wire(&*self.eng, site, after).map_err(to_err)?;
            out.append(&mut rows);
        }
        serde_json::to_string(&out).map_err(to_err)
    }

    /// Integrate a JSON array of wire rows (real Merkle-id check + blob verify +
    /// `compute_files` fold + `merge3`). Returns how many *new* rows landed.
    pub fn integrate(&self, wire_rows_json: &str) -> Result<usize, JsError> {
        let rows: Vec<WireRow> = serde_json::from_str(wire_rows_json).map_err(to_err)?;
        // Batch-integrate: fold once, not once per row (per-row is O(n²) over a
        // large catch-up / restore — a 3000-row vault was ~10s).
        let flags = self.eng.integrate_many(&rows).map_err(to_err)?;
        Ok(flags.into_iter().filter(|b| *b).count())
    }

    /// Per-file fold metadata for rich rendering: a JSON array of
    /// `{file_id, path, result_hash, merge_class, deleted, conflict}`.
    pub fn files_detail_json(&self) -> Result<String, JsError> {
        #[derive(serde::Serialize)]
        struct FileMeta<'a> {
            file_id: &'a str,
            path: &'a str,
            result_hash: Option<&'a str>,
            merge_class: &'static str,
            deleted: bool,
            conflict: bool,
        }
        let detail: Vec<FileRow> = self.eng.files_detail();
        let metas: Vec<FileMeta> = detail
            .iter()
            .map(|f| FileMeta {
                file_id: &f.file_id,
                path: &f.path,
                result_hash: f.result_hash.as_deref(),
                merge_class: f.merge_class.as_str(),
                deleted: f.deleted,
                conflict: f.conflict,
            })
            .collect();
        serde_json::to_string(&metas).map_err(to_err)
    }

    /// Sync over **iroh** (browser → relay): dial `ticket` (an iroh ticket), run
    /// the handshake + bidirectional version-vector catch-up, converge, and close.
    /// Returns a Promise resolving to the number of rows integrated from the peer.
    /// `relay_url` overrides the default public relays (e.g. a private/test relay).
    /// The whole connect+drive lives in one owned future (no borrow of `self`), so
    /// it satisfies wasm-bindgen's `'static` requirement.
    /// `on_progress(done, total)` (optional) is invoked as catch-up pages land so
    /// the UI can show real clone progress; `total` is the peer's row count (from
    /// its version vector), `done` the rows integrated so far.
    #[cfg(target_arch = "wasm32")]
    pub fn sync(
        &self,
        ticket: String,
        auth_key: Option<String>,
        relay_url: Option<String>,
        on_progress: Option<js_sys::Function>,
    ) -> js_sys::Promise {
        let eng = self.eng.clone();
        let auth_keys: Vec<String> = auth_key.into_iter().collect();
        wasm_bindgen_futures::future_to_promise(async move {
            let cb = move |done: usize, total: usize| {
                if let Some(f) = &on_progress {
                    let _ = f.call2(
                        &wasm_bindgen::JsValue::NULL,
                        &wasm_bindgen::JsValue::from_f64(done as f64),
                        &wasm_bindgen::JsValue::from_f64(total as f64),
                    );
                }
            };
            asp_core::iroh_wasm::sync_oneshot(eng, ticket, auth_keys, relay_url, &cb)
                .await
                .map(|n| wasm_bindgen::JsValue::from_f64(n as f64))
                .map_err(|e| wasm_bindgen::JsValue::from_str(&e))
        })
    }

    /// Open a **live** connection to `ticket` and keep it open: dial out once,
    /// catch up, then stream rows both ways in realtime (no polling). Remote
    /// pushes are integrated and `on_change(rows)` is invoked so the host can
    /// refresh; locally-authored rows (via `record_*`) push to the peer over the
    /// same connection. The returned Promise resolves when the connection closes,
    /// so the caller can reconnect.
    #[cfg(target_arch = "wasm32")]
    pub fn connect_live(
        &self,
        ticket: String,
        auth_key: Option<String>,
        relay_url: Option<String>,
        on_change: js_sys::Function,
    ) -> js_sys::Promise {
        let eng = self.eng.clone();
        let auth_keys: Vec<String> = auth_key.into_iter().collect();
        let (tx, rx) = futures_channel::mpsc::unbounded::<WireRow>();
        *self.live_tx.borrow_mut() = Some(tx);
        let on_change = move |n: usize| {
            let _ = on_change.call1(&wasm_bindgen::JsValue::NULL, &wasm_bindgen::JsValue::from_f64(n as f64));
        };
        wasm_bindgen_futures::future_to_promise(async move {
            asp_core::iroh_wasm::connect_live(eng, ticket, auth_keys, relay_url, rx, on_change)
                .await
                .map(|_| wasm_bindgen::JsValue::UNDEFINED)
                .map_err(|e| wasm_bindgen::JsValue::from_str(&e))
        })
    }

}

fn to_err<E: std::fmt::Display>(e: E) -> JsError {
    JsError::new(&e.to_string())
}
