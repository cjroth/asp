//! WebAssembly bindings for `asp-core` (§Implementation: the wasm/TypeScript SDK).
//! This is **not** a reimplementation — it is the *real* full engine compiled to
//! wasm. The high-level [`WasmEngine`] drives the same `MemEngine` (capture +
//! `compute_files` fold + `merge3`) and the same sans-IO `Session` (handshake +
//! version-vector catch-up) as the native daemon, so a browser/Obsidian node
//! computes byte-identical state. The low-level functions back the cross-surface
//! conformance vectors (wasm output == native output).

use asp_core::authkeys::AdmitCtx;
use asp_core::session::Step;
use asp_core::{
    compute_files, identity::ssh_pubkey_string, merge::merge3, oid, store::MemBlobStore, BlobStore,
    FileRow, Identity, LogRow, MemEngine, MergeClass, Msg, Role, Session, SessionVault, WireRow,
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
    session: Option<Session>,
}

#[derive(serde::Serialize)]
struct FeedResult {
    out: Vec<Vec<u8>>,
    integrated: usize,
    authed: bool,
    closed: Option<String>,
    /// The peer finished streaming our catch-up (`Msg::Synced`). NOT a close:
    /// a oneshot driver (SDK `Vault.sync`) ends its pass here, while a live
    /// driver (the demo's watch link) keeps the socket open for pushed rows.
    synced: bool,
}

#[wasm_bindgen]
impl WasmEngine {
    /// Create a thin node authoring as `seed`. An empty `vault_id` adopts the
    /// peer's vault on the first connect (clone).
    #[wasm_bindgen(constructor)]
    pub fn new(seed: &[u8], vault_id: &str) -> WasmEngine {
        #[cfg(target_arch = "wasm32")]
        console_error_panic_hook::set_once();
        WasmEngine { eng: std::rc::Rc::new(MemEngine::create(ident(seed), vault_id)), session: None }
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
        self.eng.record_write(path, content).map_err(to_err)?;
        Ok(())
    }

    pub fn record_remove(&self, path: &str) -> Result<(), JsError> {
        self.eng.record_remove(path).map_err(to_err)?;
        Ok(())
    }

    pub fn record_rename(&self, from: &str, to: &str) -> Result<(), JsError> {
        self.eng.record_rename(from, to).map_err(to_err)?;
        Ok(())
    }

    /// Whole-set commit from the host's current vault contents (`{path: [u8]}`).
    pub fn commit_files(&self, files_json: &str) -> Result<(), JsError> {
        let files: BTreeMap<String, Vec<u8>> = serde_json::from_str(files_json).map_err(to_err)?;
        self.eng.commit_files(&files).map_err(to_err)?;
        Ok(())
    }

    /// Stage a batch of host files (create/edit, no deletes), folding ONCE
    /// (`{path: [u8]}`). The startup reconcile uses this instead of one
    /// record_write per file — per-file re-folding is O(n²) over a large vault.
    pub fn write_files(&self, files_json: &str) -> Result<(), JsError> {
        let files: BTreeMap<String, Vec<u8>> = serde_json::from_str(files_json).map_err(to_err)?;
        self.eng.record_writes(&files).map_err(to_err)?;
        Ok(())
    }

    /// Author deletes for a JSON array of paths, folding ONCE — the startup
    /// reconcile uses this to capture files deleted while the host app was
    /// closed (no delete events fire for those; without it the peer's copy
    /// resurrects them on the next materialize).
    pub fn remove_files(&self, paths_json: &str) -> Result<(), JsError> {
        let paths: Vec<String> = serde_json::from_str(paths_json).map_err(to_err)?;
        self.eng.record_removes(&paths).map_err(to_err)?;
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

    /// Wrap locally-authored wire rows in a `Rows` data frame to send over a live
    /// (already-handshaked) connection — optimistic real-time push, the wire
    /// analogue of the native daemon pushing new rows to connected peers.
    pub fn push_frame(&self, wire_rows_json: &str) -> Result<Vec<u8>, JsError> {
        let rows: Vec<WireRow> = serde_json::from_str(wire_rows_json).map_err(to_err)?;
        Msg::Rows { rows }.to_bytes().map_err(to_err)
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
    #[cfg(target_arch = "wasm32")]
    pub fn sync(&self, ticket: String, auth_key: Option<String>, relay_url: Option<String>) -> js_sys::Promise {
        let eng = self.eng.clone();
        let auth_keys: Vec<String> = auth_key.into_iter().collect();
        wasm_bindgen_futures::future_to_promise(async move {
            asp_core::iroh_wasm::sync_oneshot(eng, ticket, auth_keys, relay_url)
                .await
                .map(|n| wasm_bindgen::JsValue::from_f64(n as f64))
                .map_err(|e| wasm_bindgen::JsValue::from_str(&e))
        })
    }

    /// Begin a connector session; returns the opening `Hello` frame to send.
    pub fn connect_start(&mut self) -> Vec<u8> {
        let ctx = AdmitCtx { no_tofu: false, auth_key_ok: false, auth_key_configured: false, default_ttl_days: 90, now_unix: 0 };
        let s = Session::new(Role::Connector, &*self.eng, Vec::new(), None, ctx);
        let frame = s
            .start()
            .into_iter()
            .find_map(|st| if let Step::Send(m) = st { m.to_bytes().ok() } else { None })
            .unwrap_or_default();
        self.session = Some(s);
        frame
    }

    /// Feed an inbound frame; returns JSON `{out:[[u8]], integrated, authed, closed, synced}`.
    pub fn feed(&mut self, frame: &[u8]) -> Result<String, JsError> {
        let msg = Msg::from_bytes(frame).map_err(to_err)?;
        let eng = &*self.eng;
        let session = self.session.as_mut().ok_or_else(|| JsError::new("no session; call connect_start"))?;
        let steps = session.on_msg(eng, msg).map_err(to_err)?;
        let mut res =
            FeedResult { out: Vec::new(), integrated: 0, authed: session.authed(), closed: None, synced: false };
        for step in steps {
            match step {
                Step::Send(m) => res.out.push(m.to_bytes().map_err(to_err)?),
                Step::Integrated(rows) => res.integrated += rows.len(),
                Step::Authenticated(_) => res.authed = true,
                Step::Closed(reason) => res.closed = Some(reason),
                // Peer finished sending our catch-up. Surfaced as its own signal —
                // not `closed` — because the DRIVER decides: a oneshot sync ends
                // its pass; a live watch link stays open for pushed rows.
                Step::PeerSynced => res.synced = true,
                // Listener-only (streamed by the native driver); a browser node is
                // never a listener, so this can't occur here.
                Step::CatchUp { .. } => {}
            }
        }
        res.authed = self.session.as_ref().map(|s| s.authed()).unwrap_or(false);
        serde_json::to_string(&res).map_err(to_err)
    }
}

fn to_err<E: std::fmt::Display>(e: E) -> JsError {
    JsError::new(&e.to_string())
}
