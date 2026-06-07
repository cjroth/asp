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
    Identity, LogRow, MemEngine, MergeClass, Msg, Role, Session, SessionVault,
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
    eng: MemEngine,
    session: Option<Session>,
}

#[derive(serde::Serialize)]
struct FeedResult {
    out: Vec<Vec<u8>>,
    integrated: usize,
    authed: bool,
    closed: Option<String>,
}

#[wasm_bindgen]
impl WasmEngine {
    /// Create a thin node authoring as `seed`. An empty `vault_id` adopts the
    /// peer's vault on the first connect (clone).
    #[wasm_bindgen(constructor)]
    pub fn new(seed: &[u8], vault_id: &str) -> WasmEngine {
        #[cfg(target_arch = "wasm32")]
        console_error_panic_hook::set_once();
        WasmEngine { eng: MemEngine::create(ident(seed), vault_id), session: None }
    }

    pub fn node_id(&self) -> String {
        self.eng.site_id()
    }

    pub fn node_ssh(&self) -> String {
        ssh_pubkey_string(&SessionVault::node_id(&self.eng), "asp")
    }

    pub fn vault_id(&self) -> String {
        SessionVault::vault_id(&self.eng)
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

    /// The materialized working tree as JSON `{path: [u8]}`.
    pub fn files_json(&self) -> Result<String, JsError> {
        let m = self.eng.files_map().map_err(to_err)?;
        serde_json::to_string(&m).map_err(to_err)
    }

    pub fn read_file(&self, path: &str) -> Result<Option<Vec<u8>>, JsError> {
        self.eng.read_file(path).map_err(to_err)
    }

    /// Begin a connector session; returns the opening `Hello` frame to send.
    pub fn connect_start(&mut self) -> Vec<u8> {
        let ctx = AdmitCtx { no_tofu: false, auth_key_ok: false, auth_key_configured: false, default_ttl_days: 90, now_unix: 0 };
        let s = Session::new(Role::Connector, &self.eng, Vec::new(), None, ctx);
        let frame = s
            .start()
            .into_iter()
            .find_map(|st| if let Step::Send(m) = st { m.to_bytes().ok() } else { None })
            .unwrap_or_default();
        self.session = Some(s);
        frame
    }

    /// Feed an inbound frame; returns JSON `{out:[[u8]], integrated, authed, closed}`.
    pub fn feed(&mut self, frame: &[u8]) -> Result<String, JsError> {
        let msg = Msg::from_bytes(frame).map_err(to_err)?;
        let eng = &self.eng;
        let session = self.session.as_mut().ok_or_else(|| JsError::new("no session; call connect_start"))?;
        let steps = session.on_msg(eng, msg).map_err(to_err)?;
        let mut res = FeedResult { out: Vec::new(), integrated: 0, authed: session.authed(), closed: None };
        for step in steps {
            match step {
                Step::Send(m) => res.out.push(m.to_bytes().map_err(to_err)?),
                Step::Integrated(rows) => res.integrated += rows.len(),
                Step::Authenticated(_) => res.authed = true,
                Step::Closed(reason) => res.closed = Some(reason),
            }
        }
        res.authed = self.session.as_ref().map(|s| s.authed()).unwrap_or(false);
        serde_json::to_string(&res).map_err(to_err)
    }
}

fn to_err<E: std::fmt::Display>(e: E) -> JsError {
    JsError::new(&e.to_string())
}
