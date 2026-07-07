//! WebAssembly bindings for `asp-core` (§Implementation: the wasm/TypeScript SDK).
//! This is **not** a reimplementation — it is the *real* full engine compiled to
//! wasm. The high-level [`WasmEngine`] drives the same `MemEngine` (capture +
//! `compute_files` fold + `merge3`) and the same sans-IO `Session` (handshake +
//! version-vector catch-up) as the native daemon, so a browser/Obsidian node
//! computes byte-identical state. The low-level functions back the cross-surface
//! conformance vectors (wasm output == native output).

use asp_core::{
    compute_files, gitgenesis, gitimport, gitwire, identity::ssh_pubkey_string, merge::merge3, oid,
    store::MemBlobStore, BlobStore, FileRow, Identity, Kind, LogRow, MemEngine, MergeClass,
    SessionVault, WireBlob, WireRow, MAIN_BRANCH_ID,
};
use std::collections::{BTreeMap, HashSet};
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

/// Map a relay `--git-proxy` base + an `https://` git URL to the two smart-HTTP
/// endpoint URLs the browser transport hits, as JSON `{base, info_refs, upload_pack}`
/// (git-bridge §7.3). Exposed so JS can assert / debug the proxy path shape; the
/// `git_clone`/`git_pull` methods build the same URLs internally.
///
/// The proxy contract (verified against `asp_core::gitproxy`):
/// `proxy path = "/git/" + <upstream host> + <upstream path>`, so
/// `("https://relay/", "https://github.com/o/r")` →
/// `https://relay/git/github.com/o/r` (then `+/info/refs?service=git-upload-pack`
/// and `+/git-upload-pack`). `.git` in the path is preserved verbatim.
#[wasm_bindgen]
pub fn git_proxy_urls(proxy_base: &str, git_url: &str) -> Result<String, JsError> {
    let base = git_proxy_base(proxy_base, git_url).map_err(|e| JsError::new(&e))?;
    #[derive(serde::Serialize)]
    struct U {
        base: String,
        info_refs: String,
        upload_pack: String,
    }
    serde_json::to_string(&U {
        info_refs: gitwire::info_refs_url(&base),
        upload_pack: gitwire::upload_pack_url(&base),
        base,
    })
    .map_err(to_err)
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

    // -------- split web persistence (rows separate from content-addressed blobs) --------
    //
    // `dump_state`/`load_state` serialize the WHOLE engine — every row AND every
    // blob's bytes — into one contiguous buffer, which on a large git clone needs
    // 2-3 full-size copies at once and OOMs wasm32's ~4 GB linear memory (the
    // "error while writing multi-byte MessagePack value" clone failure). These
    // split it: a tiny rows-only snapshot plus one content-addressed blob at a
    // time, so no single giant buffer is ever built.

    /// Rows-only web-persistence snapshot (no blob bytes). Persist the referenced
    /// blobs separately (`blob_hashes` + `get_blob`); restore them with `put_blob`
    /// before `load_rows_state`.
    pub fn export_rows_state(&self) -> Result<Vec<u8>, JsError> {
        self.eng.export_rows_state().map_err(to_err)
    }

    /// Restore an `export_rows_state` snapshot. Feed the referenced blobs back via
    /// `put_blob` (hash list from `blob_hashes_of_rows`) BEFORE calling this so
    /// branch reconciliation and the fold see their bytes. Returns rows added.
    pub fn load_rows_state(&self, bytes: &[u8]) -> Result<usize, JsError> {
        self.eng.load_rows_state(bytes).map_err(to_err)
    }

    /// The content hashes this engine holds (rows' base+result blobs, present in
    /// the store) as a JSON string array — one content-addressed OPFS entry each.
    pub fn blob_hashes(&self) -> Result<String, JsError> {
        serde_json::to_string(&self.eng.blob_hashes()).map_err(to_err)
    }

    /// The content hashes an `export_rows_state` snapshot references, decoded
    /// WITHOUT importing it (JSON string array) — so the loader can restore each
    /// blob before `load_rows_state`.
    pub fn blob_hashes_of_rows(&self, bytes: &[u8]) -> Result<String, JsError> {
        let hashes = MemEngine::blob_hashes_in_rows_state(bytes).map_err(to_err)?;
        serde_json::to_string(&hashes).map_err(to_err)
    }

    /// Store a blob (content-addressed); returns its hash. The loader feeds
    /// persisted blobs back through this before `load_rows_state`.
    pub fn put_blob(&self, bytes: &[u8]) -> Result<String, JsError> {
        self.eng.put_blob(bytes).map_err(to_err)
    }

    /// Fetch a stored blob's bytes by hash (web persistence writes these out one
    /// content-addressed entry at a time via `blob_hashes`).
    pub fn get_blob(&self, hash: &str) -> Result<Option<Vec<u8>>, JsError> {
        self.eng.get_blob(hash).map_err(to_err)
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

    // ---------------- git bridge (§7.3 web clone/pull) ----------------

    /// The git ingest-ledger summary from the fold (git-bridge §4.1), JSON
    /// `{at_sha, ingested}`: `at_sha` is the most-recently-ingested commit sha (the
    /// highest-lamport `GitIngest` row's `path`) or `null`, and `ingested` the number
    /// of ingested commits. Powers the web status chip; reads the ledger straight
    /// from the fold so it stays correct even when a native peer advances it over
    /// ordinary ASP sync. `{at_sha:null, ingested:0}` for a non-git vault.
    pub fn git_ledger_json(&self) -> Result<String, JsError> {
        let rows = all_wire_rows(&self.eng).map_err(|e| JsError::new(&e))?;
        let mut best: Option<(u64, String)> = None;
        let mut ingested = 0usize;
        for w in &rows {
            if w.row.kind == Kind::GitIngest {
                ingested += 1;
                if let Some(sha) = &w.row.path {
                    if best.as_ref().map(|(l, _)| w.row.lamport > *l).unwrap_or(true) {
                        best = Some((w.row.lamport, sha.clone()));
                    }
                }
            }
        }
        #[derive(serde::Serialize)]
        struct L {
            at_sha: Option<String>,
            ingested: usize,
        }
        serde_json::to_string(&L { at_sha: best.map(|(_, s)| s), ingested }).map_err(to_err)
    }

    /// Clone a git repo into this (pristine) vault via the relay CORS proxy
    /// (git-bridge §7.3). The wasm side owns the git protocol + import; **all HTTP
    /// lives in JS** via `fetch_fn`, so CORS/proxy plumbing stays where it's natural.
    ///
    /// `fetch_fn` (JS): `async (method, url, headers, body) => { status, body }`
    /// where `headers` is `Record<string,string>`, `body` a `Uint8Array | null`,
    /// and the result's `body` a `Uint8Array` (see `call_fetch`). The method builds
    /// the proxy URL + git request bytes, calls `fetch_fn`, and decodes the reply.
    ///
    /// Steps: GET info/refs → parse caps → POST ls-refs (HEAD symref → default branch
    /// + tip) → POST fetch `{want tip, done}` → pack → decode → plan → deterministic
    /// genesis → paged integrate under batch. `on_progress(phase, done, total)` fires
    /// with `phase ∈ {"fetching","scanning","replaying","importing","saving",
    /// "materialize"}` (in that order). Resolves to JSON
    /// `{vault_id, commits, branches, warnings, tip_sha, root_sha, remote_ref,
    /// default_branch, open_branches, refs_skipped}`.
    ///
    /// `all_branches` (the "also import open branches" checkbox,
    /// `specs/git-open-branches.md` §5): when true the ls-refs advertisement's
    /// `refs/heads/*` tips are added to the fetch wants and passed as
    /// `ImportOptions.open_branch_tips`, so every unmerged branch imports as a **live**
    /// ASP branch (phase 2 of genesis). `open_branches`/`refs_skipped` in the report
    /// count the live lanes imported and the refs skipped as already-reachable. `false`
    /// = default-branch history only (byte-identical to the base spec).
    #[cfg(target_arch = "wasm32")]
    #[allow(clippy::too_many_arguments)]
    pub fn git_clone(
        &self,
        git_url: String,
        token: Option<String>,
        proxy_base: String,
        depth: Option<u32>,
        all_branches: bool,
        fetch_fn: js_sys::Function,
        on_progress: Option<js_sys::Function>,
    ) -> js_sys::Promise {
        let eng = self.eng.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            git_clone_inner(eng, &git_url, token.as_deref(), &proxy_base, depth, all_branches, fetch_fn, on_progress)
                .await
                .map(|json| wasm_bindgen::JsValue::from_str(&json))
                .map_err(|e| wasm_bindgen::JsValue::from_str(&e))
        })
    }

    /// Pull new upstream commits into a git-cloned vault (git-bridge §4, browser
    /// fallback). Reads the ingest ledger from the fold to learn what's already
    /// imported, re-fetches the default branch, and ingests only the new commits
    /// (already-seen shas skip; a raced local edit forks + 3-way-merges, §4.2).
    ///
    /// v1 simplification: the browser has no local git object store (git-bridge §6.3),
    /// so it re-fetches a **full** self-contained pack each pull and lets
    /// `synthesize_ingest`'s `seen` set + Merkle-id dedup no-op the already-imported
    /// history — correct + deterministic, just heavier than the native incremental
    /// path. Same `fetch_fn`/`on_progress` contract as [`WasmEngine::git_clone`].
    /// Resolves to JSON `{new_commits}`.
    #[cfg(target_arch = "wasm32")]
    pub fn git_pull(
        &self,
        git_url: String,
        token: Option<String>,
        proxy_base: String,
        fetch_fn: js_sys::Function,
        on_progress: Option<js_sys::Function>,
    ) -> js_sys::Promise {
        let eng = self.eng.clone();
        wasm_bindgen_futures::future_to_promise(async move {
            git_pull_inner(eng, &git_url, token.as_deref(), &proxy_base, fetch_fn, on_progress)
                .await
                .map(|json| wasm_bindgen::JsValue::from_str(&json))
                .map_err(|e| wasm_bindgen::JsValue::from_str(&e))
        })
    }
}

fn to_err<E: std::fmt::Display>(e: E) -> JsError {
    JsError::new(&e.to_string())
}

// ===========================================================================
// git bridge — pure helpers (wasm-safe, native-checked by `--all-targets`)
// ===========================================================================
//
// These are consumed only by the wasm-gated `fetch()` transport below (and the
// native unit tests), so on a native `cargo build` they read as dead — hence the
// per-item `allow(dead_code)`, which keeps them compiled + type-checked on every
// target while staying quiet on native.

/// Rows per `integrate_many` page — a large clone/ingest streams page-by-page under
/// `set_batch` so it folds once (mirrors `gitremote::INTEGRATE_PAGE`).
#[allow(dead_code)]
const GIT_INTEGRATE_PAGE: usize = 1000;

/// Map (`--git-proxy` base, `https://` git URL) → the proxy repo base the two
/// smart-HTTP endpoints hang off (git-bridge §7.3). Verified against the
/// `asp_core::gitproxy` route: `proxy path = "/git/" + host + upstream_path`.
fn git_proxy_base(proxy_base: &str, git_url: &str) -> Result<String, String> {
    let parsed = gitwire::parse_git_url(git_url).ok_or_else(|| format!("not a git URL: {git_url}"))?;
    let base = match parsed {
        gitwire::GitUrl::Https { base } => base,
        gitwire::GitUrl::Ssh { .. } => {
            return Err("browser clone requires an https:// git URL (ssh is native-only)".into())
        }
    };
    // `base` is `https://<host>/<path>`; the proxy wants `<host>/<path>` after `/git/`.
    let host_path = base
        .strip_prefix("https://")
        .ok_or("git URL must be https:// for the browser proxy")?;
    if proxy_base.trim().is_empty() {
        return Err("git proxy base URL is not configured".into());
    }
    Ok(format!("{}/git/{}", proxy_base.trim_end_matches('/'), host_path))
}

/// The tip oid + default-branch name from an `ls-refs` response: prefer `HEAD`'s
/// symref target, else `refs/heads/main`, else the first `refs/heads/*`.
#[allow(dead_code)]
fn resolve_head(refs: &[gitwire::RefInfo]) -> Result<(String, String), String> {
    if let Some(head) = refs.iter().find(|r| r.name == "HEAD") {
        if let Some(t) = &head.symref_target {
            let db = t.strip_prefix("refs/heads/").unwrap_or(t).to_string();
            return Ok((head.oid.clone(), db));
        }
    }
    let head = refs
        .iter()
        .find(|r| r.name == "refs/heads/main")
        .or_else(|| refs.iter().find(|r| r.name.starts_with("refs/heads/")));
    match head {
        Some(r) => Ok((
            r.oid.clone(),
            r.name.strip_prefix("refs/heads/").unwrap_or(&r.name).to_string(),
        )),
        None => Err("remote advertises no HEAD / default branch (empty repo?)".into()),
    }
}

/// Bundle each row with its `base_hash`/`result_hash` blobs (from `store`) into the
/// [`WireRow`] shape `integrate_many` consumes (mirrors `gitremote::to_wires`). A
/// `base_hash` blob that lives only in the engine (not `store`) is simply omitted —
/// the fold reads it from the engine's own blob store.
#[allow(dead_code)]
fn to_wires(rows: &[LogRow], store: &dyn BlobStore) -> Vec<WireRow> {
    rows.iter()
        .map(|r| {
            let mut blobs: Vec<WireBlob> = Vec::new();
            for h in [r.base_hash.clone(), r.result_hash.clone()].into_iter().flatten() {
                if blobs.iter().any(|b| b.hash == h) {
                    continue;
                }
                if let Ok(Some(bytes)) = store.get_blob(&h) {
                    blobs.push(WireBlob { hash: h, bytes });
                }
            }
            WireRow { row: r.clone(), blobs }
        })
        .collect()
}

/// Integrate `rows` page-by-page under batch, folding once (caller enables
/// `set_batch` and `materialize`s after).
#[allow(dead_code)]
fn integrate_paged(
    eng: &MemEngine,
    rows: &[LogRow],
    store: &dyn BlobStore,
    progress: &dyn Fn(u64, u64),
) -> Result<(), String> {
    let total = rows.len() as u64;
    let mut done = 0u64;
    for chunk in rows.chunks(GIT_INTEGRATE_PAGE) {
        eng.integrate_many(&to_wires(chunk, store)).map_err(|e| e.to_string())?;
        done += chunk.len() as u64;
        progress(done, total);
    }
    Ok(())
}

#[allow(dead_code)]
fn warning_strings(warnings: &[gitimport::ImportWarning]) -> Vec<String> {
    warnings
        .iter()
        .map(|w| match w {
            gitimport::ImportWarning::Submodule { path, .. } => {
                format!("submodule at {path} imported as nothing (gitlink not materialized)")
            }
            gitimport::ImportWarning::LfsPointers { paths } => format!(
                "{} git-LFS pointer file(s) imported as pointer text (not smudged)",
                paths.len()
            ),
        })
        .collect()
}

/// The outcome of a decoded clone pack — the JSON the browser reports back.
#[allow(dead_code)]
struct CloneReport {
    vault_id: String,
    commits: usize,
    branches: Vec<String>,
    warnings: Vec<String>,
    tip_sha: String,
    root_sha: String,
    remote_ref: String,
    default_branch: String,
    /// Live open branches imported (`all_branches`, `specs/git-open-branches.md` §1).
    /// `0` for a plain clone.
    open_branches: usize,
    /// Open-branch refs skipped because already reachable from HEAD (§1). `0` for a
    /// plain clone.
    refs_skipped: usize,
}

#[allow(dead_code)]
fn clone_report_json(r: &CloneReport) -> Result<String, String> {
    #[derive(serde::Serialize)]
    struct R<'a> {
        vault_id: &'a str,
        commits: usize,
        branches: &'a [String],
        warnings: &'a [String],
        tip_sha: &'a str,
        root_sha: &'a str,
        remote_ref: &'a str,
        default_branch: &'a str,
        open_branches: usize,
        refs_skipped: usize,
    }
    serde_json::to_string(&R {
        vault_id: &r.vault_id,
        commits: r.commits,
        branches: &r.branches,
        warnings: &r.warnings,
        tip_sha: &r.tip_sha,
        root_sha: &r.root_sha,
        remote_ref: &r.remote_ref,
        default_branch: &r.default_branch,
        open_branches: r.open_branches,
        refs_skipped: r.refs_skipped,
    })
    .map_err(|e| e.to_string())
}

/// Decode a clone pack + tip into a pristine engine (git-bridge §3): pack → db →
/// plan → deterministic genesis → paged integrate under batch. All-or-nothing (rows
/// fold only after the whole pack decodes). Pure over the engine + pack bytes, so
/// this is the exact Rust half the `git_clone` transport drives — and is native-test
/// reachable from the recorded wire fixtures.
#[allow(dead_code)]
fn apply_clone_pack(
    eng: &MemEngine,
    pack: &[u8],
    tip: &str,
    default_branch: &str,
    depth: Option<u32>,
    open_branch_tips: Vec<(String, String)>,
    progress: &dyn Fn(&str, u64, u64),
) -> Result<CloneReport, String> {
    if !eng.is_pristine() {
        return Err("cannot clone a git remote into a non-empty vault".into());
    }
    // from_pack_with_progress emits the "scanning" then "replaying" phases (each
    // (done, num_objects) from the pack header); forward the phase string through.
    let db = gitimport::GitObjectDb::from_pack_with_progress(pack, gitimport::no_base_lookup, |ph, d, t| {
        progress(ph, d, t)
    })
    .map_err(|e| e.to_string())?;
    // Every open-branch candidate tip must be in the fetched pack; a missing one (the
    // server pruned the ref between ls-refs and fetch) is a clear error (mirrors the
    // native `gitremote::clone_from_git` guard).
    for (name, oid) in &open_branch_tips {
        if db.get(oid).is_none() {
            return Err(format!(
                "open branch '{name}' tip {oid} was not in the fetched pack (remote pruned it?)"
            ));
        }
    }
    // `open_branch_tips` empty ⇒ phase-1-only, byte-identical to the base spec; when the
    // "also import open branches" checkbox is on they drive phase 2 of genesis
    // (`specs/git-open-branches.md` §1–§2).
    let iopts = gitimport::ImportOptions { depth, keep_imported_branches: false, open_branch_tips };
    let plan = gitimport::plan_import(&db, tip, &iopts).map_err(|e| e.to_string())?;

    let scratch = MemBlobStore::new();
    // Genesis walks every commit; report (commits_done, commit_count) as "importing"
    // so the bar advances through a big history instead of stalling.
    let g = gitgenesis::synthesize_genesis_with_progress(
        &plan,
        &gitgenesis::DbBlobSource::new(&db),
        &scratch,
        |d, t| progress("importing", d, t),
    )
    .map_err(|e| e.to_string())?;
    eng.adopt_vault_id(&g.vault_id).map_err(|e| e.to_string())?;

    progress("saving", 0, g.rows.len() as u64);
    eng.set_batch(true);
    let res = integrate_paged(eng, &g.rows, &scratch, &|d, t| progress("saving", d, t));
    eng.set_batch(false);
    res?;
    // MemEngine materialize is a single in-memory fold (OPFS writes happen host-side
    // after), so emit the file count once to fill the "materialize" segment.
    progress("materialize", 0, 0);
    eng.materialize().map_err(|e| e.to_string())?;
    let mat_files = eng.files_detail().iter().filter(|f| !f.deleted).count() as u64;
    progress("materialize", mat_files, mat_files);

    let branches: Vec<String> = plan
        .lanes
        .iter()
        .filter(|l| l.id != gitimport::MAIN_LANE)
        .map(|l| l.name.clone())
        .collect();
    let open_branches = plan.lanes.iter().filter(|l| l.live).count();
    let refs_skipped = plan.skipped_reachable.len();
    Ok(CloneReport {
        vault_id: g.vault_id,
        commits: plan.commits.len(),
        branches,
        warnings: warning_strings(&plan.warnings),
        tip_sha: tip.to_string(),
        root_sha: plan.root_sha.clone(),
        remote_ref: format!("refs/heads/{default_branch}"),
        default_branch: default_branch.to_string(),
        open_branches,
        refs_skipped,
    })
}

/// Every wire row this engine holds, across all sites (the ledger lives in here).
#[allow(dead_code)]
fn all_wire_rows(eng: &MemEngine) -> Result<Vec<WireRow>, String> {
    let vv = SessionVault::version_vector(eng).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for site in vv.keys() {
        out.extend(SessionVault::rows_after_wire(eng, site, -1).map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// Commit shas that already have a `GitIngest` ledger row (`path` = the sha, so no
/// blob decode needed). The predicate `synthesize_ingest` uses to skip re-imports.
#[allow(dead_code)]
fn seen_shas(rows: &[WireRow]) -> HashSet<String> {
    rows.iter()
        .filter(|w| w.row.kind == Kind::GitIngest)
        .filter_map(|w| w.row.path.clone())
        .collect()
}

/// Reconstruct the imported-chain state on `main` for the repo `site` from its rows
/// (mirrors `gitremote::reconstruct_main_state`) — the tips an ongoing ingest chains
/// onto so a raced local edit forks concurrently (git-bridge §4.2).
#[allow(dead_code)]
fn build_ingest_context(
    rows: &[WireRow],
    site: &str,
    remote_ref: &str,
    seen: HashSet<String>,
) -> gitgenesis::IngestContext {
    let next_lamport = rows.iter().map(|w| w.row.lamport).max().unwrap_or(0) + 1;

    let mut site_rows: Vec<&LogRow> = rows.iter().map(|w| &w.row).filter(|r| r.site_id == site).collect();
    site_rows.sort_by_key(|r| r.seq);
    let next_seq = site_rows.iter().map(|r| r.seq).max().map(|m| m + 1).unwrap_or(0);

    let mut path_fid: BTreeMap<String, String> = BTreeMap::new();
    let mut fid_path: BTreeMap<String, String> = BTreeMap::new();
    let mut file_tip: BTreeMap<String, (String, Option<String>)> = BTreeMap::new();
    let mut main_last_row: Option<String> = None;

    for r in &site_rows {
        if r.branch_id != MAIN_BRANCH_ID {
            continue;
        }
        main_last_row = Some(r.id.clone());
        match r.kind {
            Kind::Create if r.merge_class != MergeClass::Dir => {
                if let Some(p) = &r.path {
                    path_fid.insert(p.clone(), r.file_id.clone());
                    fid_path.insert(r.file_id.clone(), p.clone());
                }
                file_tip.insert(r.file_id.clone(), (r.id.clone(), r.result_hash.clone()));
            }
            Kind::Edit => {
                file_tip.insert(r.file_id.clone(), (r.id.clone(), r.result_hash.clone()));
            }
            Kind::Rename => {
                if let Some(old) = fid_path.get(&r.file_id).cloned() {
                    path_fid.remove(&old);
                }
                if let Some(p) = &r.path {
                    path_fid.insert(p.clone(), r.file_id.clone());
                    fid_path.insert(r.file_id.clone(), p.clone());
                }
                file_tip.insert(r.file_id.clone(), (r.id.clone(), r.result_hash.clone()));
            }
            Kind::Delete => {
                if let Some(old) = fid_path.remove(&r.file_id) {
                    path_fid.remove(&old);
                }
                file_tip.insert(r.file_id.clone(), (r.id.clone(), None));
            }
            _ => {}
        }
    }

    let main_state = path_fid
        .into_iter()
        .filter_map(|(path, fid)| {
            let (row_id, content_hash) = file_tip.get(&fid)?.clone();
            Some(gitgenesis::ImportedFile { path, file_id: fid, row_id, content_hash })
        })
        .collect();

    gitgenesis::IngestContext {
        site_id: site.to_string(),
        next_seq,
        next_lamport,
        remote_ref: remote_ref.to_string(),
        main_state,
        main_last_row,
        seen,
    }
}

/// Ingest a freshly-fetched full pack's new commits into a live vault (git-bridge
/// §4.2). Pure over the engine + pack; the exact Rust half the `git_pull` transport
/// drives. Returns the number of newly-ingested commits.
#[allow(dead_code)]
fn apply_ingest_pack(
    eng: &MemEngine,
    pack: &[u8],
    new_tip: &str,
    default_branch: &str,
    progress: &dyn Fn(&str, u64, u64),
) -> Result<usize, String> {
    let all = all_wire_rows(eng)?;
    let seen = seen_shas(&all);

    progress("replaying", 0, 0);
    let db = gitimport::GitObjectDb::from_pack(pack, gitimport::no_base_lookup).map_err(|e| e.to_string())?;
    let plan = gitimport::plan_import(&db, new_tip, &gitimport::ImportOptions::default()).map_err(|e| e.to_string())?;

    let site = gitgenesis::git_site_id(&plan.root_sha);
    let remote_ref = format!("refs/heads/{default_branch}");
    let ctx = build_ingest_context(&all, &site, &remote_ref, seen);

    let scratch = MemBlobStore::new();
    let out = gitgenesis::synthesize_ingest(&plan, &ctx, &gitgenesis::DbBlobSource::new(&db), &scratch)
        .map_err(|e| e.to_string())?;
    if out.rows.is_empty() {
        return Ok(0);
    }
    progress("saving", 0, out.rows.len() as u64);
    eng.set_batch(true);
    let res = integrate_paged(eng, &out.rows, &scratch, &|d, t| progress("saving", d, t));
    eng.set_batch(false);
    res?;
    eng.materialize().map_err(|e| e.to_string())?;
    Ok(out.ledger.len())
}

/// Minimal standard-alphabet base64 (no deps) for the HTTPS `Authorization: Basic`
/// header. Git smart-HTTP hosts accept a PAT as the Basic password; GitHub's
/// convention is the `x-access-token` username.
#[allow(dead_code)]
fn base64_std(input: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | b2 as u32;
        out.push(T[((n >> 18) & 63) as usize] as char);
        out.push(T[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[((n >> 6) & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[allow(dead_code)]
fn basic_auth(token: &str) -> String {
    format!("Basic {}", base64_std(format!("x-access-token:{token}").as_bytes()))
}

// ===========================================================================
// git bridge — the wasm `fetch()`-backed transport (browser only)
// ===========================================================================

/// Build a `Record<string,string>` headers object for one git request.
#[cfg(target_arch = "wasm32")]
fn git_headers(token: Option<&str>, accept: &str, content_type: Option<&str>) -> js_sys::Object {
    let h = js_sys::Object::new();
    let set = |k: &str, v: &str| {
        let _ = js_sys::Reflect::set(&h, &JsValue::from_str(k), &JsValue::from_str(v));
    };
    set("Git-Protocol", "version=2");
    set("Accept", accept);
    if let Some(ct) = content_type {
        set("Content-Type", ct);
    }
    if let Some(t) = token.filter(|s| !s.is_empty()) {
        set("Authorization", &basic_auth(t));
    }
    h
}

/// Render a JS error value for a Rust error string.
#[cfg(target_arch = "wasm32")]
fn jerr(ctx: &str, e: &JsValue) -> String {
    let detail = e.as_string().or_else(|| js_sys::JSON::stringify(e).ok().and_then(|s| s.as_string()));
    match detail {
        Some(d) => format!("{ctx}: {d}"),
        None => ctx.to_string(),
    }
}

/// Invoke the JS `fetch_fn(method, url, headers, body)` and await its
/// `{ status, body }` result, extracting the HTTP status + response bytes.
#[cfg(target_arch = "wasm32")]
async fn call_fetch(
    fetch_fn: &js_sys::Function,
    method: &str,
    url: &str,
    headers: &js_sys::Object,
    body: Option<&[u8]>,
) -> Result<(u16, Vec<u8>), String> {
    use wasm_bindgen::JsCast;
    let body_val: JsValue = match body {
        Some(b) => js_sys::Uint8Array::from(b).into(),
        None => JsValue::NULL,
    };
    let args = js_sys::Array::new();
    args.push(&JsValue::from_str(method));
    args.push(&JsValue::from_str(url));
    args.push(headers.as_ref());
    args.push(&body_val);
    let ret = fetch_fn
        .apply(&JsValue::NULL, &args)
        .map_err(|e| jerr("fetch_fn threw", &e))?;
    let promise: js_sys::Promise = ret
        .dyn_into()
        .map_err(|_| "fetch_fn must return a Promise".to_string())?;
    let res = wasm_bindgen_futures::JsFuture::from(promise)
        .await
        .map_err(|e| jerr("fetch rejected", &e))?;
    let status = js_sys::Reflect::get(&res, &JsValue::from_str("status"))
        .ok()
        .and_then(|v| v.as_f64())
        .ok_or("fetch result missing a numeric `status`")? as u16;
    let body_js = js_sys::Reflect::get(&res, &JsValue::from_str("body"))
        .map_err(|e| jerr("reading fetch result body", &e))?;
    let bytes = if body_js.is_null() || body_js.is_undefined() {
        Vec::new()
    } else {
        js_sys::Uint8Array::new(&body_js).to_vec()
    };
    Ok((status, bytes))
}

/// A stateless git-protocol-v2 exchange: GET info/refs → POST ls-refs → return the
/// tip oid + default-branch name **and the full advertised ref list** (shared by
/// clone + pull; the ref list feeds the `all_branches` open-branch wants — §5).
#[cfg(target_arch = "wasm32")]
async fn negotiate_head(
    fetch_fn: &js_sys::Function,
    base: &str,
    token: Option<&str>,
) -> Result<(String, String, Vec<gitwire::RefInfo>), String> {
    let (st, body) = call_fetch(
        fetch_fn,
        "GET",
        &gitwire::info_refs_url(base),
        &git_headers(token, "application/x-git-upload-pack-advertisement", None),
        None,
    )
    .await?;
    if st != 200 {
        return Err(format!("git proxy GET info/refs returned HTTP {st}"));
    }
    let caps = gitwire::parse_capability_advertisement(&body).map_err(|e| e.to_string())?;
    caps.object_format().map_err(|e| e.to_string())?; // reject sha256 up front

    let ls_req = gitwire::build_ls_refs(&["HEAD", "refs/heads/"]);
    let (st2, body2) = call_fetch(
        fetch_fn,
        "POST",
        &gitwire::upload_pack_url(base),
        &git_headers(
            token,
            "application/x-git-upload-pack-result",
            Some("application/x-git-upload-pack-request"),
        ),
        Some(&ls_req),
    )
    .await?;
    if st2 != 200 {
        return Err(format!("git proxy ls-refs returned HTTP {st2}"));
    }
    let refs = gitwire::parse_ls_refs_response(&body2).map_err(|e| e.to_string())?;
    let (tip, default_branch) = resolve_head(&refs)?;
    Ok((tip, default_branch, refs))
}

/// POST a `fetch {want …, done}` and return the demuxed packfile bytes. `wants` is the
/// full want set — a single tip for a plain clone/pull, or HEAD + every open-branch
/// tip for an `all_branches` clone (one negotiation, one pack; §6).
#[cfg(target_arch = "wasm32")]
async fn fetch_pack(
    fetch_fn: &js_sys::Function,
    base: &str,
    token: Option<&str>,
    wants: &[String],
    depth: Option<u32>,
) -> Result<Vec<u8>, String> {
    let fr = gitwire::FetchRequest {
        wants: wants.to_vec(),
        done: true,
        deepen: depth,
        ..Default::default()
    };
    let (st, body) = call_fetch(
        fetch_fn,
        "POST",
        &gitwire::upload_pack_url(base),
        &git_headers(
            token,
            "application/x-git-upload-pack-result",
            Some("application/x-git-upload-pack-request"),
        ),
        Some(&fr.build()),
    )
    .await?;
    if st != 200 {
        return Err(format!("git proxy fetch returned HTTP {st}"));
    }
    let resp = gitwire::FetchResponseParser::parse(&body).map_err(|e| e.to_string())?;
    if !resp.saw_packfile {
        return Err("git fetch returned no packfile (negotiation-only response)".into());
    }
    Ok(resp.pack)
}

#[cfg(target_arch = "wasm32")]
#[allow(clippy::too_many_arguments)]
async fn git_clone_inner(
    eng: std::rc::Rc<MemEngine>,
    git_url: &str,
    token: Option<&str>,
    proxy_base: &str,
    depth: Option<u32>,
    all_branches: bool,
    fetch_fn: js_sys::Function,
    on_progress: Option<js_sys::Function>,
) -> Result<String, String> {
    let progress = |phase: &str, d: u64, t: u64| {
        if let Some(f) = &on_progress {
            let _ = f.call3(
                &JsValue::NULL,
                &JsValue::from_str(phase),
                &JsValue::from_f64(d as f64),
                &JsValue::from_f64(t as f64),
            );
        }
    };
    if !eng.is_pristine() {
        return Err("cannot clone a git remote into a non-empty vault".into());
    }
    let base = git_proxy_base(proxy_base, git_url)?;

    progress("fetching", 0, 0);
    let (tip, default_branch, refs) = negotiate_head(&fetch_fn, &base, token).await?;

    // Open-branch candidates (`all_branches`, `specs/git-open-branches.md` §1/§6):
    // every advertised `refs/heads/*` except the default branch, fetched in the SAME
    // pack (single negotiation). The planner decides unique-vs-skipped-reachable.
    let mut wants: Vec<String> = vec![tip.clone()];
    let mut open_candidates: Vec<(String, String)> = Vec::new();
    if all_branches {
        for r in &refs {
            let Some(short) = r.name.strip_prefix("refs/heads/") else { continue };
            if short == default_branch {
                continue;
            }
            open_candidates.push((short.to_string(), r.oid.clone()));
            if !wants.contains(&r.oid) {
                wants.push(r.oid.clone());
            }
        }
    }

    let pack = fetch_pack(&fetch_fn, &base, token, &wants, depth).await?;

    let report = apply_clone_pack(&eng, &pack, &tip, &default_branch, depth, open_candidates, &progress)?;
    clone_report_json(&report)
}

#[cfg(target_arch = "wasm32")]
async fn git_pull_inner(
    eng: std::rc::Rc<MemEngine>,
    git_url: &str,
    token: Option<&str>,
    proxy_base: &str,
    fetch_fn: js_sys::Function,
    on_progress: Option<js_sys::Function>,
) -> Result<String, String> {
    let progress = |phase: &str, d: u64, t: u64| {
        if let Some(f) = &on_progress {
            let _ = f.call3(
                &JsValue::NULL,
                &JsValue::from_str(phase),
                &JsValue::from_f64(d as f64),
                &JsValue::from_f64(t as f64),
            );
        }
    };
    let base = git_proxy_base(proxy_base, git_url)?;

    progress("fetching", 0, 0);
    // The web pull follows the default branch only (snapshot semantics for open
    // branches, `specs/git-open-branches.md` §4/§5): open branches imported at clone
    // are ordinary ASP branches and are NOT re-synced here. The `seen`-set dedup in
    // `apply_ingest_pack` still stops duplicate rows, but the web path does NOT do the
    // §4 merge-after-import re-attachment — the native driver does that via
    // `synthesize_ingest_with_open_branches`/`reconstruct_imported_branches`. So if an
    // imported open branch later merges upstream, the web pull imports the merge as a
    // NEW lane rather than attaching it to the existing imported branch (benign
    // duplicate-history, base-spec §4.3 class). Native follow-up is documented work.
    let (new_tip, default_branch, _refs) = negotiate_head(&fetch_fn, &base, token).await?;

    // Cheap up-to-date short-circuit: the tip already has a ledger row.
    let already = seen_shas(&all_wire_rows(&eng)?);
    if already.contains(&new_tip) {
        return Ok(r#"{"new_commits":0}"#.to_string());
    }

    let pack = fetch_pack(&fetch_fn, &base, token, &[new_tip.clone()], None).await?;
    let n = apply_ingest_pack(&eng, &pack, &new_tip, &default_branch, &progress)?;
    Ok(format!(r#"{{"new_commits":{n}}}"#))
}

// ===========================================================================
// native unit tests (pure git-bridge helpers — no wasm runtime)
// ===========================================================================

#[cfg(test)]
mod git_tests {
    use super::*;

    #[test]
    fn proxy_base_maps_host_and_path() {
        assert_eq!(
            git_proxy_base("https://relay.example", "https://github.com/owner/repo").unwrap(),
            "https://relay.example/git/github.com/owner/repo"
        );
        // trailing slash on the proxy + `.git` in the path are preserved.
        assert_eq!(
            git_proxy_base("https://relay.example/", "https://github.com/owner/repo.git").unwrap(),
            "https://relay.example/git/github.com/owner/repo.git"
        );
        // the two endpoint URLs match the gitproxy route shape exactly.
        let base = git_proxy_base("https://r", "https://github.com/o/r").unwrap();
        assert_eq!(
            gitwire::info_refs_url(&base),
            "https://r/git/github.com/o/r/info/refs?service=git-upload-pack"
        );
        assert_eq!(gitwire::upload_pack_url(&base), "https://r/git/github.com/o/r/git-upload-pack");
    }

    #[test]
    fn proxy_base_rejects_ssh_and_junk() {
        assert!(git_proxy_base("https://r", "git@github.com:o/r").is_err());
        assert!(git_proxy_base("https://r", "not a url").is_err());
        assert!(git_proxy_base("", "https://github.com/o/r").is_err());
    }

    #[test]
    fn git_proxy_urls_json_shape() {
        let s = git_proxy_urls("https://r/", "https://github.com/o/r").unwrap();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["info_refs"], "https://r/git/github.com/o/r/info/refs?service=git-upload-pack");
        assert_eq!(v["upload_pack"], "https://r/git/github.com/o/r/git-upload-pack");
    }

    #[test]
    fn resolve_head_prefers_symref() {
        let refs = vec![
            gitwire::RefInfo {
                oid: "a".repeat(40),
                name: "HEAD".into(),
                symref_target: Some("refs/heads/trunk".into()),
                peeled: None,
            },
            gitwire::RefInfo { oid: "a".repeat(40), name: "refs/heads/trunk".into(), symref_target: None, peeled: None },
        ];
        assert_eq!(resolve_head(&refs).unwrap(), ("a".repeat(40), "trunk".to_string()));
    }

    #[test]
    fn resolve_head_falls_back_to_main() {
        let refs = vec![gitwire::RefInfo {
            oid: "b".repeat(40),
            name: "refs/heads/main".into(),
            symref_target: None,
            peeled: None,
        }];
        assert_eq!(resolve_head(&refs).unwrap(), ("b".repeat(40), "main".to_string()));
        assert!(resolve_head(&[]).is_err());
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_std(b""), "");
        assert_eq!(base64_std(b"f"), "Zg==");
        assert_eq!(base64_std(b"fo"), "Zm8=");
        assert_eq!(base64_std(b"foo"), "Zm9v");
        assert_eq!(base64_std(b"foobar"), "Zm9vYmFy");
        assert!(basic_auth("tok").starts_with("Basic "));
    }
}
