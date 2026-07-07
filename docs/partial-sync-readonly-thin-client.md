# ASP Partial Sync, Read-Only Subdirs, and Thin Remote-View Client — Implementation Plan

## 1. Feasibility verdict

The three features are one spectrum of *replication scope × write capability × locality*:

```
full replica         partial replica        read-only replica         thin remote-view
(every peer today)   (A: subdir subset,     (B: pull a subdir but      (C: NO local log;
 whole log + blobs    still folded locally)   forbidden to push it)      reads/writes served
 folded locally)                                                          by a source node)
```

| Feature | Effort | Single biggest obstacle |
|---|---|---|
| **A — Partial subdir sync** | **L** (single-subdir, star-leaf) / XL (general whitelist, multi-upstream) | The version vector is a single **dense per-site `MAX(seq)` watermark** (`version_vector`, sqlite.rs:363). A filtered replica has holes, so its VV lies about completeness. Solvable **only** if the scoped replica is a pull-only *leaf* pinned to one upstream. |
| **B — Read-only subdir** | **S–M** (Trust mode / star) · **L** (Verified mode / mesh, opt-in) | Read-only's strength is a **per-vault choice** (§4.4). In **Trust mode** it is enforced topologically by a single integrator (free, star-only); in a mesh Trust mode degrades to advisory (anti-entropy re-shares a blocked peer's rows through any laxer node, fanout iroh_net.rs:343 + session.rs:283-286; `integrate_many` proves only hash↔fields, never authorship — `id_valid`, engine.rs:482). In **Verified mode** it is cryptographic and holds in a true mesh via optional ed25519 signing whose only cost is a one-time verify on clone. |
| **C — Thin remote-view client** | **L–XL** | There is **no request/response or query verb** in the protocol — `Msg` is pure row/vector streaming (wire.rs:45-80). Every read API folds a *local* full store (`state_as_of`/`file_at` call `all_rows()`, engine.rs:1347/1370). C is a net-new transport + attribution channel + the deliberate loss of offline/local-first. |

**Recommended first build:** ship **A's single-subdir clone gated by B's read-only flag together**, as one deliverable — *"a read-only, single-subdir clone pulled from a hub"*. That is the highest-value, lowest-risk slice (single-user multi-device + enterprise star both want exactly this), needs **no `PROTO` bump and no signatures**, and reuses one new schema migration. **C is the follow-on** and *reuses A's path-scoping and B's write-authorization directly* because it runs in the same star with the same `node_id`-keyed `authorized_keys` policy.

**Signing is a user-selectable per-vault mode, not a fixed decision (§4.4).** A vault is born in **Trust mode** (default — the zero-crypto star above) or **Verified mode** (opt-in — mandatory ed25519 signing + author→path checks that make read-only *and* A's subdir-read confidentiality hold in a true P2P mesh). The choice is per-vault and inherited by every clone; the cost — a one-time ~1.5–4 s verify per 100k-row clone, zero for reads/edits/live-sync — is paid only by vaults that opt into the mesh guarantee. It must stay a *mode*, never per-row optionality (the downgrade attack, §4.4).

The load-bearing good news, verified against the code: **all three are pure whole-row *selection*.** No feature rewrites `LogRow.path`, renumbers `seq`, or adds a `LogRow` field, so `canonical_fields()`/`merkle_id` are byte-identical and **no vault forks** (log.rs:219-235).

---

## 2. Background — the constraints (all verified in-tree)

**Dense-seq version vector, derived not persisted.** `version_vector()` is `SELECT site_id, MAX(seq) FROM log GROUP BY site_id`, recomputed every call (sqlite.rs:363-372). Catch-up asks `rows_after(site, seq > peer_vv[site])` (sqlite.rs:343-348, page variant :354-360). The implicit invariant is *"VV[site] = N ⇒ I hold the dense prefix 0..=N"*. `seq` is dense on author (`next_seq` = `MAX(seq)+1`) but receive is `INSERT OR IGNORE ... log` keyed by Merkle id (append_row sqlite.rs:208-219, append_rows :229-249) — a receiver silently stores `{0,1,2,5}` with a hole and never complains. **No gap detection exists anywhere.** A dead `peer_state(site_id,last_seq)` table is declared (sqlite.rs:58) but has zero readers/writers.

**Catch-up cursor advances on the *unfiltered* page.** In `stream_catchup` (iroh_net.rs:400-432) the paging `cursor` is taken from `page.last().row.seq` at **:417**, *before* the blob-dedup `page[*].blobs.retain(|b| sent_blobs.insert(b.hash))` at **:419-421**, then `push_rows_chunked` and a final `Msg::Synced` (:423-431). This ordering is the seam that makes A's dense-seq story work (§3).

**Path needs the fold.** Only `Create` and `Rename` carry `path: Some(..)` (engine.rs:344 / rename site); `Edit`/`Delete`/`Reclass` carry `path: None` (engine.rs:323; log.rs:172-173). The file_id→path map *is* the fold: seeded by Create (fold.rs:130), mutated only by Rename (fold.rs:168), materialized into the `files` table. A per-row prefix test therefore cannot classify the common case.

**Fold orphans on a split chain.** In `apply_rows`, `Edit`/`Rename`/`Delete`/`Reclass` do `let Some(st) = states.get_mut(&r.file_id) else { continue }` (fold.rs:142/164/174/181). If a file's `Create` is missing, **every later row is silently skipped and the file never materializes.** Non-file kinds (`Merge`/`Branch`/`Tag`/`GitCommit`/`GitIngest`/`GitPlan`) are fold no-ops (fold.rs:196-201) — but `Branch` rows drive `fork_vv` visibility that `state_as_of`/`file_at` depend on (integrate reconciles them *before* folding, engine.rs:466/503), so dropping any corrupts the fold. `resolve_paths` emits tombstones **with their path** (fold.rs:286-297); `unique_path` only suffixes the basename, never the directory prefix (fold.rs:344), so subdir membership is invariant under ` (n)` collision resolution.

**Binary admission, no policy dimensions.** `AdmitCtx`/`AdmitDecision`/`AuthKey` (authkeys.rs:16-77) carry identity + expiry + provenance only — no path, direction, or read-only. `decide_admission` returns `Admit | Insert(source) | Deny` (authkeys.rs:41-61). The admit result is **discarded** at the call site — `session.rs:251` only checks `Err`. The `Session` holds the transport-verified `peer_node` (session.rs:94, set :245) but never threads it into integrate.

**Row signatures are 100% inert.** `sig` is ed25519 over `signing_payload()` which frames the same `canonical_fields()` (log.rs:238) — and is **excluded** from `canonical_fields` (log.rs:183-184, 219-235), so populating it does *not* change Merkle ids. `Identity::sign`/`verify_detached` exist (identity.rs:34/81) but `verify_detached` has **zero non-test callers**; every builder writes `sig: vec![]` (e.g. engine.rs:326/347); nothing verifies on integrate.

**Branches replicate to all peers; visibility is a fold-time filter.** Rows carry `branch_id`; `state_as_of`/`file_at` filter with `vis.sees(r)` at fold time (engine.rs:285/1347/1370). This is the precedent for a *materialize-time* view filter (used by A's rename-out handling), and the warning that a fold-time filter is **not** a sync/trust boundary.

**Existing read APIs, all in-process, all fold a full local store.** `state_as_of(t)` and `file_at(path,t)` fold `all_rows()` then read blobs (engine.rs:1344-1358 / 1367+) — O(whole log) per call. HEAD reads go through the indexed `files` table (`live_files` sqlite.rs:649, `live_file_by_path` :396) — but all three live-file helpers filter `deleted=0` (sqlite.rs:399/652/674).

**Existing request/response surface: essentially none for ASP data.** `Msg` is Hello/Vector/Rows/Push/Denied/Bye/Synced (wire.rs:45-80). The only HTTP server in-tree is the git CORS proxy — native `hyper` (`hyper::server::conn::http1`, `service_fn`, `TokioIo`, `TcpListener` at gitproxy.rs:67-73; accept loop :149-162), a workspace dep (Cargo.toml:51-52) — but it forwards exactly two upstream git shapes and reads *nothing* of the vault. The `asp relay` is a pure ciphertext forwarder. The MCP debug tools are a local filesystem bridge, not a networked-vault read. **A vault query verb is new.**

**`PROTO = 4`** (wire.rs:24); the `Hello` proto check hard-refuses any mismatch (session.rs:198-206). `Hello.auth_key` is already `#[serde(default)]` (wire.rs:60) — so an optional field is *wire*-compatible, but a scoped connector must not *silently* receive a full clone from an un-upgraded listener, which is why client-*requested* scope would still want a bump. We avoid this by making scope **server-granted** (§3).

---

## 3. Feature A — Partial subdir sync

### 3.1 Chosen approach

**A single-subdir (v1) or path-prefix clone, enforced as a listener-side SEND filter, with the grant stored server-side in `authorized_keys` and keyed by the peer's transport-verified `node_id`.** The scoped node is a *dumb connector*: it negotiates nothing, sends its normal `Hello` and VV, and simply receives what the listener chose to send. This means **no `Hello` field, no new `Msg` variant, no `PROTO` bump** — the listener already may send "what it chose," which the protocol permits.

The filter operates on **whole `file_id` chains**, never per-row path, resolved from the listener's own fold. This is the only way to satisfy both fold-completeness (never split a Create from its descendants) and `resolve_paths` correctness (a complete path-prefix cut).

Three hard invariants make the dense-seq model correct *without* a new receiver cursor table:

1. **The scoped replica is a pull-only LEAF pinned to exactly ONE upstream.** It must never serve catch-up to a third peer and never add a second upstream. Its sparse `MAX(seq)` VV is therefore *never advertised to anyone as authoritative* — so the "third peer gets a permanent hole" hazard (the fatal flaw both the protocol-correct and pragmatic critics found) cannot occur, and there is no need to override `version_vector()` on a listener path (the listener path always runs on a full node). Enforced by a local `partial` flag set at clone time that (a) refuses the `Listener` role / `watch --listen`, (b) refuses a second peer, (c) labels the UI "partial."
2. **Membership is monotonic "ever resolved under X"** (§3.3) so the send set never *un-sends* a file — no tombstone synthesis, no forged local deletes.
3. **Scope-widening is an explicit re-clone**, not a live backfill (§3.2).

### 3.2 The VV / dense-seq solution

At a **fixed scope with one upstream**, the existing `MAX(seq)` model is already correct, because of the cursor-before-filter ordering:

- The listener's `stream_catchup` advances its paging `cursor` from the *unfiltered* `page.last().row.seq` at **iroh_net.rs:417**, so the examined frontier moves across dropped seqs while the shipped set stays sparse.
- On reconnect the scoped replica advertises `version_vector() = MAX(held)` and asks `seq > MAX(held)`; the listener re-runs the *same deterministic* filter over the tail. Permanently-out-of-scope rows are simply never delivered — no re-request loop, no hole bug.

**The filter MUST be inserted between line 417 and line 419** — after the cursor advance, **before** `sent_blobs.retain` — because each `WireRow` self-carries its `base_hash`/`result_hash` blobs (wire.rs:34-43), and the dedup marks a content hash "sent" on first sight. If we filtered *after* the dedup, a dropped out-of-scope row would mark a *shared* content blob sent, and a later in-scope row referencing the same hash would ship blob-less → the receiver folds `unwrap_or_default()` empty bytes (fold.rs:117-122) and silently diverges. Filtering first means each surviving row keeps its own bundled blobs and dedup only collapses duplicates *among survivors*.

```rust
// iroh_net.rs stream_catchup, between :417 and :419
cursor = page.last().map(|w| w.row.seq as i64).unwrap_or(cursor);   // :417 (unchanged)
page.retain(|wr| scope.admits_row(wr, &members));                    // NEW: whole-chain membership
for wr in &mut page { wr.blobs.retain(|b| sent_blobs.insert(b.hash.clone())); } // :419-421 (now over survivors)
```

**Scope-widening cannot backfill via the VV** — the now-in-scope rows sit *below* the receiver's `MAX(seq)` watermark and are unrequestable (`rows_after` is strictly `seq > peer_vv`). This is handled as an admin action = **re-clone at the wider scope** (or reset the replica's advertised vector for affected sites to force a full re-stream; `append_rows` INSERT-OR-IGNORE dedupes already-held rows, sqlite.rs:229, so it costs only re-transfer). Named limitation, documented in `--subdir` help.

**Multi-upstream partial sync is out of scope for v1** and deferred to the PROTO-5 protocol-correct variant (per-scope-tagged frontiers on the wire). The v1 leaf constraint forbids it structurally.

### 3.3 Rename-across-boundary handling

Two membership definitions, both needed:

- **SYNC membership (which rows to ship) = "the file_id EVER resolved under X."** Computed on the listener by scanning that file's `Create`/`Rename` rows (`rows_for_file`, sqlite.rs:329) through `Scope::allowed` (§3.6). Monotonic: once in, always in. This is what makes rename correct:
  - *Rename INTO X* (`Create src/…`, later `Rename docs/…`): the file is a member via the in-scope Rename, so its **whole chain ships including the out-of-scope Create** → the fold materializes it (no orphan at fold.rs:164).
  - *Rename OUT of X* (`Create docs/…`, later `Rename src/…`): still a member (monotonic), so the replica **receives the Rename-out row and learns the file left** — no stale ghost, and no cross-boundary ghost-edit (the pragmatic critic's "edit lands on the moved file" trap is closed because the replica sees the move).
- **VIEW membership (what the replica displays) = "current folded path under X,"** applied as a **materialize-time filter mirroring branch visibility** (`vis.sees` at engine.rs:285). A file that has left X stays in the log but is hidden from disk/UI.

**Membership must include deleted tombstones.** `file_ids_under(prefix)` must query the raw `files` table (which persists tombstones with their path, fold.rs:286-297) **without** a `deleted=0` clause — do **not** reuse `live_files`/`live_file_by_path`/`file_id_for_path` (all filter `deleted=0`, sqlite.rs:399/652/674), or an in-scope Delete's file_id drops out of the set, the Delete never ships, and the receiver keeps a live ghost of a deleted file.

**Realtime rename-into-scope (the hazard both critics flagged as unsolved).** A lone `Msg::Push` for a Rename that brings a file into scope would arrive without its below-watermark Create → `else continue` → the file vanishes forever. The fix lives in scope-aware fanout (§3.5): for a **`Rename`** row that makes a file a member of a conn's scope, the listener ships that file's **whole chain via `rows_for_file` as a `Msg::Rows` batch**, not the lone `Push`. This is idempotent (INSERT-OR-IGNORE) so it is also safe for rename-*within*-scope, and it delivers the below-watermark Create out-of-band so the fold completes. `Create`/`Edit`/`Delete`/`Reclass` for an already-member file ship as normal lone `Push`es.

### 3.4 Non-file rows and causal ancestors

**Ship all non-file rows wholesale, never path-filter them.** `Branch`/`Tag`/`Merge`/`GitCommit`/`GitIngest`/`GitPlan` are fold no-ops (fold.rs:196-201) but their `path` field is overloaded (branch metadata; commit SHA for Git*), so a prefix test misclassifies them, and `Branch` records are load-bearing for `fork_vv` visibility (reconciled before fold, engine.rs:466/503). They are few and carry real seqs, so shipping all keeps the examined-frontier cursor coherent. Causal ancestors of in-scope files are covered for free: whole-`file_id`-chain shipping carries every parent row, and each row self-carries its base/result blobs (wire.rs:34-43).

**Git-bridge hazard — must close (unaddressed by the pragmatic design).** A partial fold synthesizes a partial *root* tree (`build_tree_object` with `prefix=""` over the whole fold), which on push **deletes every out-of-scope file on the remote** and breaks byte-identical push determinism. The real danger is not local push but **`GitPlan` authorship**: a plan authored on a partial replica syncs to full nodes that then push the partial tree. Therefore a `partial` replica must **hard-disable git push AND suppress the interval auto-plan policy** (mirror the browser's push-disable and the per-remote `frozen` gate). This falls out of the pull-only-leaf flag.

### 3.5 Hook points (fn + file:line)

| Seam | Location | Change |
|---|---|---|
| **Primary catch-up filter** | `stream_catchup`, crates/asp-core/src/iroh_net.rs:400-432 | Insert `page.retain(scope)` between :417 and :419 (§3.2). Thread the conn's `PeerPolicy` in via `Step::CatchUp` (below). |
| **Realtime filter + rename backfill** | `fanout`, crates/asp-core/src/net.rs:53-60 + callers iroh_net.rs:343 (`Step::Integrated`), net.rs:96 (watcher) | Extend `Conns` value from `mpsc::UnboundedSender<Msg>` (net.rs:45) to `(sender, PeerPolicy)`; skip out-of-scope rows; for a member-making `Rename`, send `rows_for_file` as `Msg::Rows` instead of the lone `Push` (§3.3). Must live **in fanout**, not only catch-up (the hub re-forward at :343 is the leak path). |
| **Inline catch-up (connector push-back / in-process / wasm-served)** | `catchup_rows`, crates/asp-core/src/session.rs:116-126 | Apply the same whole-chain filter, or this path leaks. |
| **Retain the grant** | `Session` struct session.rs:79-97; admit call session.rs:250-258 | Add `policy: PeerPolicy` field; store the admitted grant instead of discarding it. Thread it into `Step::CatchUp { peer_vv, policy }` (session.rs:72/284). |
| **Membership resolver** | new `Engine::file_ids_under(prefix)` over the raw `files` table (index `files_path`, sqlite.rs:51) + `SqliteStore::rows_for_file` (sqlite.rs:329) for the "ever under X" scan | No `deleted=0` filter (§3.3). |
| **Whitelist matcher** | new `Scope::allowed(rel) -> bool` beside `Scope::ignored` (scope.rs:68), delegating to the existing `glob_match` | §3.6. |
| **Materialize-time view filter** | fold path, mirroring `vis.sees` at engine.rs:285 | Hide files whose current path left X. |

### 3.6 `Scope::allowed`

`Scope::ignored` is a denylist (scope.rs:68-80) with an un-negatable `ALWAYS_IGNORE_DIRS` guard (scope.rs:20/70). A subdir allow-list is its complement; expressing it via the `*` + `!subdir/**` inversion has a bare-dir footgun (`!subdir/**` doesn't re-include the `subdir` row itself). Add a first-class `Scope::allowed(rel) -> bool` sibling delegating to the existing memoized, DoS-hardened `glob_match` (the `failed` HashSet already defends against a hostile synced `.aspignore`). `scope.rs` is in the always-compiled section (lib.rs:28), std-only, so the same predicate runs in the wasm node.

### 3.7 Multi-surface plumbing

- **CLI:** `asp authorize <pubkey> --subdir PATH` (main.rs:170 struct + handler ~main.rs:386) stores the grant; `asp clone --subdir PATH` (main.rs:103) records only the local `partial` leaf flag.
- **Desktop:** one new Tauri command via the f6c1d07 5-file ceremony (DesktopEngine method; `commands.rs` pass-through; `generate_handler!` registration desktop/src-tauri/src/lib.rs:91; `api.ts` interface + both impls; extend the invoke-arg guard **desktop/bun-isolated/tauriApi.git.test.ts**). DTOs `#[serde(rename_all="camelCase")]`.
- **wasm:** mirror the whole-chain filter in the `MemEngine` catch-up path so a browser node can never become the laxer leak; `Scope::allowed` already compiles to wasm.

**Effort: L** (single-subdir, monotonic membership, star leaf, no PROTO bump). XL for general whitelisting + strict current-scope membership + multi-upstream frontier-on-wire.

---

## 4. Feature B — Read-only subdirs

### 4.1 Chosen approach and the honest trust model

B = A's read-scoped subdir clone + a per-peer **write refusal**, whose *strength* is set by the vault's **security profile** (§4.4). The same feature yields three honestly-distinct guarantee levels: the first is an anti-pattern to avoid, the other two are the user-selectable modes.

1. **Advisory (mesh peers, Verified-mode rules NOT enforced) — NOT a real boundary, never ship this.** A "read-only" peer pushes to a laxer node B, B integrates and fans out (iroh_net.rs:339-345), and the restricting node A pulls it back via Vector anti-entropy (session.rs:283-286). A cannot even tell the row "came from" the blocked peer: rows carry the original author's `site_id` and `integrate_many` checks only `id_valid()` (engine.rs:482), which proves hash↔fields consistency, **not authorship**. This is exactly what you get if signing is left *per-row optional and unsigned is accepted* — the downgrade trap (§4.4). Only ever surface it in the UI as best-effort, never as a security boundary.
2. **Trust mode (star, no signatures) — a real boundary via topology, and the default.** The read-only peer connects *only* to the source; the source is the single integrator and rejects the peer's inbound rows using the QUIC-verified `peer_node` (session.rs:94/245). The peer cannot relay around the single integrator. Zero crypto cost. This is exactly Feature C's topology and the single-user-hub topology. The guarantee is a *deployment* constraint (the star must actually be a star), not a cryptographic one — see §10 risk 2.
3. **Verified mode (mesh, mandatory signatures) — a cryptographic boundary that holds in true P2P, opt-in.** ed25519 sigs mandatory *within this vault* (sign at every builder, `verify_detached` + author→path check at `integrate`, including the wasm `MemEngine`). Read-only — and A's subdir-read confidentiality — then hold regardless of topology because the check travels with the data. This is a **user-selectable per-vault mode** (§4.4), enabled at genesis (cheap) or via an epoch-grace migration (the real work) — *not* a deferred someday-project, and **not** a `PROTO` bump. Its only cost is the one-time verify-on-clone in §4.4.

### 4.2 Enforcement point (fn + file:line)

Gate `integrate_batch` on the retained `PeerPolicy` **before** it calls `vault.integrate_many` — in the `Msg::Rows` arm (session.rs:289-295) and `Msg::Push` arm (session.rs:296-302):

```rust
// session.rs Msg::Rows / Msg::Push, before integrate_batch (:311)
if self.policy.read_only && rows.iter().any(|wr| is_file_mutation(&wr.row)) {
    return Ok(vec![Step::Send(Msg::Denied { reason: "read-only".into() }),
                   Step::Integrated(vec![])]); // empty so the connector's own catch-up still completes
}
```

`is_file_mutation` = `kind ∈ {Create,Edit,Rename,Delete,Reclass}`. This lives in the **sans-IO `Session`**, so both the native `Engine` and the wasm `MemEngine` enforce identically — mandatory, or the browser becomes the laxer node. It needs **no** change to the `integrate_many` trait signature (the `Session` already holds `peer_node`, session.rs:94). Path-granular read-only (a read-write *sub*-grant inside the subdir) is B-plus and needs A's file_id→path resolver; whole-connection read-only is v1.

For the eventual regime (3), the deeper choke is `Engine::integrate`/`integrate_many` (engine.rs:452/480, beside `id_valid` at :453/482), where a `peer: &NodeId` param + `verify_detached` + author→path check would be added.

### 4.3 Phase-1 shippable

**Trust mode is phase-1:** reuse A's `authorized_keys.read_only` column + `PeerPolicy` retention; add the ~one-branch reject in `on_msg` + the `MemEngine` mirror + a CLI `--read-only` on `authorize`. No signatures, no engine-trait change, no PROTO bump — the vault stays in the default Trust profile. **Verified mode (§4.4)** is a distinct, later opt-in that layers signing on top of this same reject; it does not block phase 1.

**Effort: S–M** (Trust mode, on top of A). Verified mode is **L** on top (§4.4) — an opt-in profile, not a fixed cost every deployment pays.

### 4.4 Security profiles: Trust vs Verified (optional signing)

Signing is a **per-vault mode the user chooses**, and one frozen design decision makes it clean: **`sig` is excluded from the Merkle id** (`canonical_fields()` omits it; the `sig_does_not_affect_id` test pins it, log.rs:183/346). A signed row and the same row unsigned have the **identical id**, so signing is additive metadata — turning it on or off never changes ids, never breaks content-addressing or dedup, and **never forks a vault**. `signing_payload()` frames exactly those identity fields (log.rs:238), so a signature authenticates precisely what the id commits to.

**Two selectable modes, set per vault:**

| | **Trust mode** (default = today) | **Verified mode** (opt-in) |
|---|---|---|
| Author signs | no (`sig: vec![]`, engine.rs:326/347) | yes, every mutating row |
| Integrate check | `id_valid()` only (engine.rs:482) | `id_valid()` + `verify_detached` + author→path ACL; **unsigned/wrong-author → rejected** |
| Enforcement basis | topological (single integrator) + connection read-only | cryptographic — travels with the data |
| Safe topology | star only | true P2P mesh |
| Crypto cost | zero | one-time verify on clone (below) |

**It MUST stay a per-vault mode, never per-row "signed or not, both accepted" — that is the downgrade attack.** If any node accepts unsigned rows, an attacker (or an old client) strips the `sig` and re-sends; the unsigned copy launders through the lenient node and flows back to strict nodes via anti-entropy (the §4.1 advisory leak, one level up). So Verified-mode nodes **reject** unsigned/unauthorized rows fleet-wide — one lenient node breaks the guarantee for everyone.

**The mode lives at genesis, inherited by every clone.** A local config flag is insufficient — a fresh clone that doesn't know it should be strict would accept unsigned rows (silent downgrade). Bake the profile into the **vault genesis / manifest** so every replica learns it as part of cloning, matching the repo's set-at-birth, identity-bearing pattern (branch defaults, git-bridge domains). At the connection layer, advertise the profile in `Hello` as an **additive** field (like `auth_key`, already `#[serde(default)]`, wire.rs:60) so a Verified node **refuses or warns** on a peer that would feed it unsigned rows instead of silently dropping them. This keeps it **additive — no forced `PROTO` bump**: Trust mode is byte-identical to today, and Verified mode just populates a wire field that already exists and enforces locally.

**Enforce at integrate, not at fold.** Both the signature verify and the author→path ACL run once, at row entry (`integrate_many`, engine.rs:482, beside the existing `id_valid()`), so the stored log is "already trusted" and every subsequent fold / `state_as_of` / `file_at` pays **zero** crypto. (This tightens the "ACL at every fold" phrasing — do it at the write boundary so a 100k-row fold on every vault open never repeats it.)

**Performance envelope (Verified mode).** Signing is once per *your* edit (~15–20 µs, dwarfed by the SQLite write + fsync already in `record_write`) — imperceptible. Verification is once per row **per node, on first receipt** — never repeated. For a cold clone of a 100k-change project: ~4 s single-threaded, ~1.5 s with `ed25519-dalek::verify_batch` over each `CATCHUP_PAGE_ROWS` page, ~0.4 s batched across cores — and it overlaps the network transfer that already dominates a clone. Steady-state live sync (a few rows per push) is sub-millisecond; re-opening / folding / history queries pay nothing (log already verified). Storage/wire: +64 bytes/row (~6.4 MB for 100k). The mesh guarantee's cost is a one-time, clone-only burst, paid only by vaults that opt in.

**Turning an existing Trust vault into Verified** is the one genuinely hard part: its historical rows are unsigned, and a strict node would reject its own history (authors may be gone and cannot re-sign). Resolve with a **signing epoch** — grandfather every row before a cutoff frontier as trusted, require signatures only after it — idiomatically the same "pre-migration grace" the repo already uses (`authkeys.rs` admits `expires_at IS NULL`; pre-branching rows default to `main`). Choosing Verified **at genesis** avoids this entirely. Verified → Trust is a deliberate security *downgrade* — gate it behind an explicit, logged admin action, never a casual toggle.

**wasm parity is mandatory:** the `MemEngine` integrate path must sign/verify identically, or the browser becomes the lenient node that defeats Verified mode.

*Deferred extension — per-subdir profile* ("this sensitive dir is Verified, the rest Trust"): feasible on top of A's path scoping, but the verify decision becomes path-dependent (needs the fold), so ship **whole-vault** mode first.

---

## 5. Feature C — Thin remote-view client

### 5.1 Chosen approach

A source node exposes read/query + write + subscribe over a **separate iroh ALPN (`asp/query/1`)**, running alongside — and independent of — the row-streaming sync ALPN. The thin client keeps **no local log or blobs**: every read is answered by a *server-side fold*, every write is *authored by the source*. Running on a separate ALPN leaves `Msg`/`PROTO` untouched (no bump) and makes thin-view an opt-in server capability.

**Why ALPN over an HTTP sidecar** (the pragmatic design's alternative): the source's authorization is keyed by iroh `node_id` (`authorized_keys`), and the QUIC handshake already proves the client's `node_id` (session.rs:245). So the query ALPN **reuses A's `allowed_paths` and B's `read_only` directly, with no separate bearer-token→policy table** and no signatures — the exact "C reuses A and B" the brief asks for. It also traverses the existing relay, so a browser wasm client can reach it. *(If a plain-HTTP gateway is later needed for non-iroh clients, the vendored `hyper` stack (gitproxy.rs:67-73) is the model, plus a `node_id`/token→policy bridge — offered as an optional deployment, not the primary path.)*

The star is **naturally enforced**: a thin client speaks *only* the query ALPN, never the sync ALPN, so it is not a sync participant at all — there is no client-to-client row path to disable, and no `fanout` carve-out to get wrong.

### 5.2 Read/query + subscribe protocol

New frame types in their own module (not `wire.rs::Msg`), dispatched by a new `ThinSession` handler parallel to `Session::on_msg` (session.rs:195), driven from a new accept loop beside `serve` (iroh_net.rs:445):

- `Query{ id, ListDir{path} | ReadFile{path} | ReadFileAt{path,ts} | Stat{path} }` → `QueryResp{ id, .. }`, answered from the source's full store: HEAD via `live_files` (sqlite.rs:649, indexed, cheap); as-of via `state_as_of`/`file_at` (engine.rs:1344/1367 — O(whole log), acceptable for occasional history-slider use). Every result is filtered by the client's `allowed_paths` grant (A).
- `Submit{ id, Write{path,bytes} | Rename{from,to} | Delete{path}, envelope_sig, nonce }` → `SubmitResp{ id, result }` (§5.3).
- `Subscribe{ sub_id, path_prefix }` / `Event{ sub_id }` / `Unsubscribe{ sub_id }` — **signal-then-pull** for v1 (§5.4).

### 5.3 Write-through, authorship, attribution, causal validity

The client **cannot** author under its own `site_id`: `record_write` hardcodes `site_id: self.site_id()` + a dense `next_seq` (engine.rs:313/292), and two writers on one `site_id` collide on `UNIQUE(site_id,seq)` (sqlite.rs:28) and defeat version-vector catch-up (the single-writer invariant). And `canonical_fields()` is frozen (log.rs:219-235) — no `authored_by` field can be added to the row without forking every vault.

So **the source authors the row on the client's behalf** via `Engine::record_write`/`record_remove`/`record_rename` (engine.rs:298/369/397); the row is causally valid and convergent (a normal sealed row under the source's `site_id`), and it fans out to full sync peers through the existing conns path. History legitimately says *"the source authored it."* Per-user attribution rides **outside the row**:

- The client signs the `Submit` **envelope** (`path + bytes + nonce`) with its ed25519 identity; the source verifies it with `verify_detached` (identity.rs:81) against the client's `authorized_keys` `node_id` **before** authoring — the first real use of the currently-inert verify path, scoped to just thin-client submits (no fleet-wide rollout, no PROTO bump; the row's own `sig` stays empty in Trust mode — in a Verified-mode vault the source signs the row it authors with its **own** key, §4.4).
- The source records `(row_id → client_node_id, envelope_sig, ts)` in a **node-local, never-synced** `remote_edits` table (§6). Blame/audit joins it. Honest limitation: other replicas and the derived git author line see only "source authored"; cryptographic attribution is source-local.

**`.aspignore` no-op trap (must handle):** `record_write` returns `Ok(None)` for an ignored path (engine.rs:299-301). The `Submit` handler must detect `None` and return an explicit error, not a silent success.

**Lost-update trap (open, mitigated):** `record_write` builds a *linear* Edit against the source's current tip, so two thin clients editing one path serialize as source-side last-writer-wins (the 3-way merge only triggers across divergent parents). Mitigation: the client sends the `base_hash` it read; the source rejects with a conflict if the tip moved (optimistic concurrency), letting the client re-read and retry.

### 5.4 Live updates

v1 = **signal-then-pull**, mirroring the desktop `vault-changed` pattern: hook `Engine::set_change_listener`/`notify_change` (engine.rs:442/446, fired in `integrate_many` :518) to emit a bare `Event{sub_id}` to each subscriber whose scope intersects the change; the client re-queries the affected subtree via `live_files` (indexed, cheap). This avoids computing path-level folded deltas per change (which would be O(fold) — `notify_change` only signals today, engine.rs:446). Path-level `Delta{path→bytes|tombstone}` frames are a later optimization once profiling justifies them.

### 5.5 Offline / latency tradeoff

C **abandons offline and local-first**: no local log ⇒ no offline read/write, every read is a round-trip, reads are source-authoritative (no client optimistic fold/merge), and the source is a single point of failure. This is the deliberate enterprise "one single-source-of-truth node" trade — full/partial replicas remain the offline-capable path. Optional mitigation: a small LRU of recently-read blobs/state for **read-only** offline viewing (never offline authoring).

### 5.6 Hooks and surface order

Reads: `state_as_of`/`file_at`/`live_files` (engine.rs:1344/1367, sqlite.rs:649). Writes: `record_write`/`record_remove`/`record_rename` (engine.rs:298/369/397). Subscribe: `set_change_listener`/`notify_change` (engine.rs:442/446). Auth: `authorized_keys` `allowed_paths`/`read_only` (A/B). Attribution: `verify_detached` (identity.rs:81) + new `remote_edits` table.

**Surface order:** build the **source** first (`asp serve` opening the `asp/query/1` ALPN on a native node), smoke-test with a CLI `asp view <ticket> --paths` query client, then add the **web thin-client backend** (an alternate `api.ts` implementation targeting the query ALPN over the relay — the browser already speaks iroh via wasm). **Desktop stays a full replica** (it wants offline); the web app is the natural first thin client.

**Effort: L–XL.** Reuses the transport, engine read/write, notify plumbing, and A/B authorization; net-new is the query/submit/subscribe frames + `ThinSession` handler + envelope-sig attribution + the thin client backend.

---

## 6. Data model / schema changes

All new state is **node-local, never synced**, added with the house convention (append to `SCHEMA` for fresh DBs + guarded `ALTER` for existing DBs; no version table).

**`authorized_keys` gains two columns** (A + B). Add to the `CREATE TABLE` in `SCHEMA` (sqlite.rs:60-63) for fresh DBs, and a new `migrate_authz()` for existing DBs, modeled **exactly** on `migrate_branching` (sqlite.rs:157-182) / `migrate_git_push` (sqlite.rs:186-198): read `PRAGMA table_info(authorized_keys)` into a `HashSet`, then guarded `ALTER TABLE ... ADD COLUMN` only if absent. Wire into `init` at sqlite.rs:147-148.

```sql
ALTER TABLE authorized_keys ADD COLUMN allowed_paths TEXT;              -- glob JSON; NULL = full
ALTER TABLE authorized_keys ADD COLUMN read_only INTEGER NOT NULL DEFAULT 0;
```

Extend `AuthKey` (authkeys.rs:64-77), `authkey_from`/`insert_authkey` (sqlite.rs ~909/922), and `engine.authorize(..)`. Surface the grant through `AdmitCtx`/`decide_admission` (authkeys.rs:18/41) and **retain it on the `Session`** (fix the discard at session.rs:251).

**`remote_edits` (C):** node-local attribution side table, appended to `SCHEMA` as `CREATE TABLE IF NOT EXISTS`:

```sql
CREATE TABLE IF NOT EXISTS remote_edits(
  row_id TEXT PRIMARY KEY, client_node_id TEXT, envelope_sig BLOB, submitted_at INTEGER);
```

**Security profile (§4.4) is a genesis property, not a synced table row.** Store the `Trust | Verified` mode — and, if a Trust→Verified migration was performed, the signing-epoch cutoff (a lamport/frontier watermark) — in the vault manifest / genesis record so it is inherited by every clone and cannot be locally downgraded. It is *read* on the integrate path to decide whether to verify. A node-local mirror, if convenient, follows the house convention (`CREATE TABLE IF NOT EXISTS signing_epoch(...)`), but the authoritative copy is the inherited genesis property, not per-node config.

**Do not repurpose the dead `peer_state` table (sqlite.rs:58)** — its intent is unconfirmed; use fresh, purpose-named tables. A per-`(peer,scope)` receiver cursor is **not** needed in v1 (the leaf constraint, §3.1); it belongs only to the deferred multi-upstream PROTO-5 variant.

---

## 7. Frozen-rule & PROTO impact

**`canonical_fields()` / `oid::merkle_id` do NOT change and must NOT** (log.rs:219-235). All of A/B are pure whole-row *selection* — never rewrite `path` (field index 10) or renumber `seq` (index 2), both of which are hashed; C authors *normal* rows via `record_write` (sealed normally). Populating `sig` is safe (it is excluded from `canonical_fields`, log.rs:183) — Trust mode leaves it empty, and **Verified mode (§4.4) fills it yet still does not fork**, because the id ignores it. **No vault forks.** This is the single most important safety property and it holds under inspection.

**`PROTO` stays 4** for the recommended slice:
- A: scope is server-granted in `authorized_keys` and enforced by the listener; the connector's wire behavior is unchanged, and `Hello` is untouched (session.rs:186-193). No bump.
- B (star): an integrate-time reject + node-local schema. No bump.
- C: a **separate ALPN** (`asp/query/1`) leaves the sync `Msg`/`PROTO` framing untouched. No bump.
- Security profile (§4.4): the `Hello` mode advert is an additive `#[serde(default)]` field and `sig` already exists on the wire, so **Trust and Verified are both `PROTO` 4**. Verified mode changes *local enforcement*, not framing; an old peer is simply refused admission to a Verified vault, gracefully.

**`PROTO` → 5 is required only for one deferred hard variant:** client-*requested* scope negotiated in `Hello` (multi-upstream partial sync with per-scope frontiers). That is a coordinated same-day fleet upgrade that hard-refuses old peers at Hello (session.rs:198-206) — the documented v3/v4 discipline (wire.rs:12-23) — touching the fly.io vault + web demo + every client together. Not identity-forking, but a hard compatibility break; keep it out of v1. **Verified mode is *not* in this bucket** (§4.4): its `Hello` advert is additive and an old peer is simply refused admission to a Verified vault without a framing break.

---

## 8. Phased roadmap

**Phase 0 — Policy plumbing (S).** `migrate_authz()` + `authorized_keys.{allowed_paths, read_only}` (§6); extend `AuthKey`/`authkey_from`/`insert_authkey`/`engine.authorize`; retain the admitted `PeerPolicy` on the `Session` (fix session.rs:251). *Unblocks A, B, and C's authorization.*

**Phase 1 — Read-only whole-peer (S–M).** B regime (2): the `on_msg` reject (session.rs:289-302) + `MemEngine` mirror + `asp authorize --read-only` + parity test. *Ships one-way sync from a hub today; no scoping yet.*

**Phase 2 — Single-subdir clone (L).** A's whole-`file_id`-chain filter in `stream_catchup` (between iroh_net.rs:417 and :419) + `catchup_rows` (session.rs:116) + scope-aware `fanout` with rename-into-scope whole-chain reship (net.rs:53) + `Scope::allowed` + `file_ids_under` (raw `files`, tombstones kept) + the materialize-time view filter + the `partial` pull-only-leaf flag (refuse listener/second-upstream, suppress git push + GitPlan). CLI `--subdir` + one desktop command + wasm parity. **Phase 1 + Phase 2 together = "read-only single-subdir clone from a hub"** — the recommended first deliverable.

**Phase 3 — Thin remote-view client (L–XL).** C: `asp/query/1` ALPN + `ThinSession` + query/submit/subscribe frames; server-authored write-through with signed-envelope attribution + `remote_edits`; signal-then-pull subscription; web thin-client backend. *Reuses Phase 0's `allowed_paths`/`read_only` and Phase 2's fold-membership resolver directly.*

**Phase 3.5 — Verified security profile (L, opt-in, any time after Phase 1).** The mesh trust boundary as a **user-selectable per-vault mode** (§4.4), not a deferred someday-project: genesis-set `Trust | Verified`; sign in `record_write`/`record_remove`/`record_rename`; `verify_detached` + author→path ACL at `integrate_many` (engine.rs:482); the additive `Hello` mode advert; `MemEngine` parity; batch-verify on the clone path. **No `PROTO` bump.** The signing-epoch grace for a Trust→Verified migration is the extra increment; choosing Verified at genesis skips it. *Unblocks true P2P (non-star) read-only + subdir-read confidentiality.*

**Phase 4 (deferred) — Multi-upstream partial sync (XL).** `PROTO`→5, client-requested `Subscription` in `Hello`, per-`(peer,scope)` frontier cursor table + a scope-tagged completeness signal on the wire. Build only if a scoped replica must pull from more than one upstream (the v1 leaf constraint, §3.1, forbids it).

---

## 9. Test strategy

Per `.claude/skills/verification-playbook`: house style is **deterministic LCG fuzz inside ordinary `#[test]`s — no proptest, no cargo-fuzz**; hermetic `tempfile::tempdir()` + `Identity::from_seed`; the three high-leverage patterns are ground-truth invariant, byte-determinism, and N-vs-2N scaling.

**A — ground-truth invariant (the load-bearing test).** A subdir-scoped replica's fold **==** a full replica's fold **restricted to the in-scope file_ids**, byte-for-byte, over a deterministic LCG history of create/edit/rename-across-boundary/delete. Drive it through the in-process two-session pump (session.rs test pattern) so it exercises the real `stream_catchup`/`catchup_rows` filter, not a mock.

**A — dense-seq regression.** Filter drops a mid-sequence slice ⇒ assert convergence within scope and that the receiver never re-requests forever (`{0,1,2,5}` with 3,4 permanently out-of-scope stays converged). Separately: rename-into-scope at **realtime** (a lone Push) ⇒ assert the whole chain is reshipped and the file materializes (guards fold.rs:164).

**A — blob-dedup ordering.** Two files sharing one content blob, one in-scope one out; assert the in-scope file's bytes are correct (guards the iroh_net.rs:419 ordering trap). **A — tombstone membership.** Delete an in-scope file; assert the Delete ships and the receiver shows no ghost (guards the `deleted=0` helper trap).

**A — N-vs-2N scaling.** A vault of N in-scope + N out-of-scope files ⇒ the scoped clone transfers ~N rows/blobs, not ~2N (proves the filter actually reduces footprint, not just the view).

**B — negative test at the enforcing edge.** In-process pump: listener marks the connector `read_only`; assert the connector's authored row **never** appears in the listener's log, while the listener's rows **do** reach the connector (one-way verified). Mirror in the `MemEngine` path (the browser-parity regression the critics flagged as most likely).

**Verified mode — downgrade + enforcement.** In a Verified-profile vault: (1) an unsigned mutating row is **rejected** at `integrate_many` on every surface incl. `MemEngine` (the downgrade attack — strip the sig, assert it does not land); (2) a validly-signed row from an author *not* authorized for that path is rejected; (3) a signed, authorized row converges normally; (4) byte-determinism — the same content signed vs unsigned yields the **same Merkle id** (guards the sig-excluded-from-id invariant, log.rs:346); (5) epoch grace — pre-cutoff unsigned history is accepted while post-cutoff unsigned rows are rejected. Exercise the batch-verify path via a ≥1-page clone.

**C.** `Submit` round-trips to exactly one source-authored `LogRow` (`site_id` = source) + one `remote_edits` row; a bad envelope sig is rejected; an out-of-grant `Query`/`Submit` is refused; a subscribed client gets an `Event` after an unrelated peer's in-subtree edit; an `.aspignore` path returns an explicit error (not `Ok(None)`); the optimistic base_hash guard rejects a stale write.

**Flaky-e2e protocol.** Any networked-lane failure must be **baselined against a clean worktree at HEAD** before it is attributed to this change (the iroh/relay lane is flaky under VM load regardless of the diff, per AGENTS.md); run the networked lane single-threaded and rebuild `target/release/asp` first.

**Cross-surface soak.** After Phase 2, run the `sync-soak-test` harness (CLI `asp watch --listen` + desktop engines + fuzzed file ops) with one scoped participant to assert convergence + live UI update within scope and no out-of-scope leakage.

---

## 10. Risks & open questions (ranked)

1. **A's correctness is single-upstream-leaf-only.** The `MAX(seq)` model is correct *only* while the scoped replica pulls from one upstream and never serves. If the `partial` flag is missing or a scoped node is peered with a second/laxer node, it either receives out-of-scope rows (leak) or advertises a sparse VV as complete and hands a third peer a permanent hole. **This flag is the entire correctness guarantee** — enforce it structurally (refuse `Listener` role and second upstream) and test it, don't rely on a boolean alone. Multi-upstream is deferred to Phase 4, not "supported with caveats."

2. **B's strength depends on the vault's security profile (§4.4).** In **Trust mode** it is topological — a real boundary only in an enforced star; in a mesh it degrades to advisory (anti-entropy routes a blocked peer's edits back through any laxer node; `integrate` cannot prove authorship, `id_valid` only, engine.rs:482). In **Verified mode** it is cryptographic and holds in a true mesh. The UI must state the active mode — "read-only, enforced by this hub" vs "read-only, cryptographically enforced" — and never over-claim. The failure mode to avoid is **advisory**: Verified-mode rules half-applied while peers mesh (risk 9).

3. **Realtime rename-into-scope is the subtle A bug.** A lone Push for a boundary-crossing Rename orphans the file unless the whole chain is reshipped (§3.3). This is the one place fanout must do more than skip — verify it with the realtime rename test.

4. **Cross-surface parity is a doubled, easily-skewed surface.** Every A filter and B reject must be mirrored in the wasm `MemEngine` Session/integrate path, or the browser becomes the laxer node that defeats both. Mandatory parity tests.

5. **A's confidentiality leaks cross-boundary path *names* under renames.** Because `path` is frozen and the fold needs the Create/Rename rows, a file that ever transited X ships a row bearing its out-of-X path. Monotonic membership discloses transited files' historical path names to a replica scoped to X. Honest downgrade: subdir *read* scoping is a footprint/organization boundary and a star confidentiality boundary, **not** a guarantee that no out-of-subtree path string is ever visible.

6. **C abandons offline/local-first and makes the source a SPOF.** Inherent to "one single-source-of-truth node," not a bug — but it must be an explicit product decision, and desktop should remain a full replica.

7. **C attribution is source-local.** `remote_edits` is never synced and the row says "source authored." If auditors need in-log per-user attribution, that is impossible without a vault fork (frozen `canonical_fields`); set expectations up front. The signed envelope gives cryptographic *source-local* non-repudiation, which is the best available without forking.

8. **C concurrent-write semantics.** Server-side LWW per path unless the optimistic base_hash guard is implemented; decide whether silent clobber or explicit-conflict is the product behavior (§5.3).

9. **The downgrade attack is Verified mode's failure mode (§4.4).** If any node accepts unsigned rows, a stripped-signature copy launders through it and defeats the guarantee fleet-wide. Signing must be a per-vault *mode* that **rejects** unsigned rows — never per-row optionality — the mode must be genesis-inherited so a fresh clone can't silently downgrade, and `MemEngine` must enforce identically. This is the single thing to get right if Verified mode is built.

10. **Trust→Verified migration on an existing vault is the hard increment.** Historical rows are unsigned; without a signing-epoch grace cutoff a strict node rejects its own history. The epoch is idiomatic (pre-migration-grace precedent) but must be chosen deliberately; Verified-at-genesis avoids it entirely. Verified→Trust is a security downgrade — gate it behind an explicit, logged action.

**Open questions for product before implementation:**
- Is single-subdir sufficient for v1, or is a general path-set whitelist required at launch? (Single-subdir is L; general whitelist pushes A toward XL.)
- Default vaults to **Trust mode** (star, zero-crypto) with **Verified mode** (mesh, signed) as a per-vault opt-in (§4.4) — or is Verified the expected default for the target ICP? (Recommendation: Trust default; Verified opt-in at genesis, +L effort, no PROTO bump.)
- Does scope need to be **client-requested** (self-service collaborator sharing → `Hello` field → PROTO 5), or is **admin-granted** server-side scope enough for the multi-device + enterprise ICP? (v1 assumes admin-granted.)
- For C's first client: web thin-backend over the query ALPN (recommended), or a plain-HTTP gateway (needs a token→policy bridge but no wasm-iroh)?
