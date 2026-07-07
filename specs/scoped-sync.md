# Spec: Scoped sync — RBSR anti-entropy, partial & read-only replicas, thin remote-view clients

Consolidates two specs into one arc:
- `specs/anti-entropy-rbsr.md` — replacing the version-vector reconciliation
  primitive with **range-based set reconciliation (RBSR)**.
- `docs/partial-sync-readonly-thin-client.md` — **partial subdir** replicas (A),
  **read-only subdir** replicas (B), and a **thin remote-view client** (C).

They belong together because they share one substrate and one failure mode: the
**dense-seq version vector**. The partial-sync features (A/B/C) currently work
*around* the VV's dense-prefix lie with a single-upstream-leaf constraint; RBSR
(§2) *removes* that lie at the substrate, so it is the enabler the partial-sync
design's Phase 4 (multi-upstream) anticipated. This document is the single source
of truth for both; it supersedes the two originals.

**Decisions.** RBSR (Chris, 2026-07-07) — motivated by the deep-research finding
that iroh-docs, Willow, and p2panda, the three projects closest to ASP's
transport, independently converged on RBSR, and by the VV dense-seq hole this doc
documents. Scoped sync A/B/C — recommended first build is *"a read-only,
single-subdir clone pulled from a hub"* (§0), no `PROTO` bump, delivered with the
same verification layers as the git bridge.

---

## 0. Goal, scope, non-goals

**The replication-scope spectrum** (features A/B/C):

```
full replica         partial replica        read-only replica         thin remote-view
(every peer today)   (A: subdir subset,     (B: pull a subdir but      (C: NO local log;
 whole log + blobs    still folded locally)   forbidden to push it)      reads/writes served
 folded locally)                                                          by a source node)
```

| Feature | Effort | Single biggest obstacle |
|---|---|---|
| **RBSR anti-entropy** | **M–L** | None blocking — the row `id` is already the `log` PK, so the range index RBSR needs already exists. Complexity is the multi-round state machine + a sound (non-XOR) fingerprint. |
| **A — Partial subdir sync** | **L** (single-subdir, star-leaf) / XL (general whitelist, multi-upstream — relaxed by RBSR, §2.5) | The version vector is a dense per-site `MAX(seq)` watermark (`version_vector`, sqlite.rs:441). A filtered replica has holes, so its VV lies about completeness. Correct **either** with the pull-only-leaf constraint (§3.1) **or**, generally, on RBSR (§2.5). |
| **B — Read-only subdir** | **S–M** (Trust mode / star) · **L** (Verified mode / mesh, opt-in) | Read-only's strength is a **per-vault choice** (§4.4). Trust mode = topological (single integrator, star-only, free); Verified mode = cryptographic (opt-in ed25519 signing, holds in a true mesh). |
| **C — Thin remote-view client** | **L–XL** | There is **no request/response or query verb** in the sync protocol — `Msg` is pure row/vector streaming (wire.rs:46-80). Every read API folds a *local* full store. C is a net-new query ALPN + attribution channel + the deliberate loss of offline/local-first. |

**Recommended first build:** ship **A's single-subdir clone gated by B's
read-only flag together** — *"a read-only, single-subdir clone pulled from a
hub"* — the highest-value, lowest-risk slice (single-user multi-device +
enterprise star both want exactly this), needing **no `PROTO` bump and no
signatures**. **RBSR (§2) is a parallel, foundational track** that can land before
or after this slice; it is a *prerequisite* for the general (multi-upstream,
non-leaf) form of A. **C is the follow-on** and reuses A's path-scoping and B's
write-authorization directly.

**In scope:** the RBSR reconciliation primitive (wire, session, store, drivers);
partial-subdir send-filtering; read-only enforcement and the optional Verified
security profile; the thin-client query ALPN; the schema, PROTO, roadmap, test,
and risk analysis for all of it.

**Non-goals / frozen guarantees:**
- **No change to row identity, fold order, or merge.** RBSR reconciles the *set*
  of rows; A/B are pure whole-row *selection*; C authors *normal* rows. None
  rewrites `LogRow::canonical_fields()`, `oid::merkle_id` framing, `path`, or
  `seq`, so ids are byte-identical and **no vault forks** (log.rs:219-235). This
  is the single most important safety property (§7).
- **No forced `PROTO` bump** for the recommended slice or for RBSR (§7).
- **Not** multi-upstream partial sync in v1 of A (deferred; RBSR is its enabler).

---

## 1. Background — the shared constraints (all verified in-tree)

**Dense-seq version vector, derived not persisted.** `version_vector()` is
`SELECT site_id, MAX(seq) FROM log GROUP BY site_id`, recomputed every call
(sqlite.rs:441). Catch-up asks `rows_after(site, seq > peer_vv[site])`
(sqlite.rs:421, page variant :432). The implicit invariant is *"VV[site] = N ⇒ I
hold the dense prefix 0..=N."* `seq` is dense on author (`next_seq` =
`MAX(seq)+1`) but receive is `INSERT OR IGNORE ... log` keyed by Merkle id
(append_rows sqlite.rs:259) — a receiver silently stores `{0,1,2,5}` with a hole
and never complains. **No gap detection exists anywhere.** A dead
`peer_state(site_id,last_seq)` table is declared (sqlite.rs:58) but has zero
readers/writers. *This is the flaw RBSR (§2) removes and the constraint A works
around (§3.1).*

**Why it holds today anyway:** rows arrive dense-in-order over a single ordered
QUIC stream from a single upstream, in causal-push order, so holes don't arise in
the current full-replica star. The moment a peer has two upstreams, a filtered
feed (A), or any out-of-order `Push`, the invariant breaks.

**The row id is already a range key.** `log.id TEXT PRIMARY KEY` (sqlite.rs:24) —
the merkle id — so `WHERE id>=?1 AND id<?2 ORDER BY id` is index-backed with **no
new index**. `log_site ON log(site_id, seq)` (sqlite.rs:31) still serves VV + the
fold.

**Catch-up cursor advances on the *unfiltered* page.** In `stream_catchup`
(iroh_net.rs:400-432) the paging `cursor` is taken from `page.last().row.seq` at
**:417**, *before* the blob-dedup `page[*].blobs.retain(|b|
sent_blobs.insert(b.hash))` at **:419-421**, then `push_rows_chunked` and a final
`Msg::Synced` (:431). This ordering is the seam that makes A's dense-seq story
work (§3.2).

**Path needs the fold.** Only `Create` and `Rename` carry `path: Some(..)`
(engine.rs:344 / rename site); `Edit`/`Delete`/`Reclass` carry `path: None`. The
file_id→path map *is* the fold: seeded by Create (fold.rs:130), mutated only by
Rename (fold.rs:168), materialized into the `files` table. A per-row prefix test
therefore cannot classify the common case.

**Fold orphans on a split chain.** In `apply_rows`,
`Edit`/`Rename`/`Delete`/`Reclass` do `let Some(st) = states.get_mut(&r.file_id)
else { continue }` (fold.rs:142/164/174/181). If a file's `Create` is missing,
**every later row is silently skipped and the file never materializes.** Non-file
kinds (`Merge`/`Branch`/`Tag`/`GitCommit`/`GitIngest`/`GitPlan`) are fold no-ops
(fold.rs:196-201) — but `Branch` rows drive `fork_vv` visibility that
`state_as_of`/`file_at` depend on (integrate reconciles them *before* folding,
engine.rs:466/503), so dropping any corrupts the fold. `resolve_paths` emits
tombstones **with their path** (fold.rs:286-297); `unique_path` only suffixes the
basename, never the directory prefix (fold.rs:344), so subdir membership is
invariant under ` (n)` collision resolution.

**Binary admission, no policy dimensions.** `AdmitCtx`/`AdmitDecision`/`AuthKey`
(authkeys.rs:16-77) carry identity + expiry + provenance only — no path,
direction, or read-only. The admit result is **discarded** at the call site —
`session.rs:251` only checks `Err`. The `Session` holds the transport-verified
`peer_node` (session.rs:94, set :245) but never threads it into integrate.

**Row signatures are 100% inert.** `sig` is ed25519 over `signing_payload()`
which frames the same `canonical_fields()` (log.rs:238) — and is **excluded**
from `canonical_fields` (log.rs:183-184, 219-235), so populating it does *not*
change Merkle ids. `Identity::sign`/`verify_detached` exist (identity.rs:34/81)
but `verify_detached` has **zero non-test callers**; every builder writes `sig:
vec![]`; nothing verifies on integrate. *This exclusion is what makes Verified
mode (§4.4) additive and fork-free.*

**Branches replicate to all peers; visibility is a fold-time filter.** Rows carry
`branch_id`; `state_as_of`/`file_at` filter with `vis.sees(r)` at fold time
(engine.rs:285/1347/1370). This is the precedent for a *materialize-time* view
filter (A's rename-out handling), and the warning that a fold-time filter is
**not** a sync/trust boundary.

**Existing read APIs fold a full local store.** `state_as_of(t)` and
`file_at(path,t)` fold `all_rows()` then read blobs (engine.rs:1344-1358 /
1367+) — O(whole log) per call. HEAD reads go through the indexed `files` table
(`live_files` sqlite.rs:649, `live_file_by_path` :396) — all three live-file
helpers filter `deleted=0` (sqlite.rs:399/652/674).

**Existing request/response surface: essentially none for ASP data.** `Msg` is
Hello/Vector/Rows/Push/Denied/Bye/Synced (wire.rs:46-80). The only HTTP server
in-tree is the git CORS proxy (gitproxy.rs). The `asp relay` is a pure ciphertext
forwarder. **A vault query verb is new (C).**

**`PROTO = 4`** (wire.rs:24); the `Hello` proto check hard-refuses any mismatch
(session.rs:198-206). `Hello.auth_key` is already `#[serde(default)]`
(wire.rs:60) — so additive optional `Hello` fields are wire-compatible.

---

## 2. RBSR anti-entropy — the substrate fix

Today's anti-entropy is a **one-shot version-vector exchange**: each side sends
`Msg::Vector { site_id → MAX(seq) }` once after auth (session.rs:263/274), then
ships every row with `seq > peer_vv[site]`. RBSR replaces the *reconciliation
primitive* with one driven by **actual set membership**, immune to the dense-seq
hole (§1). Well-synced pairs cost **one round-trip** (top fingerprints match);
divergence costs ≈ `log(n)` rounds.

### 2.1 Reconcile set and fingerprint

**Reconcile set.** All log rows a node holds, keyed by their merkle `id` (32-byte
SHA-256, stored hex). Total order = lexicographic over the hex id — a single flat
id-space across all sites and kinds. RBSR does **not** know `site_id`/`seq`, which
is exactly why it is immune to the dense-prefix hole.

**Fingerprint over `[lo, hi)`.** An **incremental, associative, commutative**
combiner over the per-id digests in the range: identical row-sets ⇒ identical
fingerprint on every node (byte-determinism, same ethos as the fold); queryable
without re-hashing the whole range each round; empty range = the group identity.

**⚠️ Do NOT use plain XOR of the ids.** XOR is incremental and commutative but
**collides**: two different id-sets can XOR-cancel to the same value, so we would
judge a range "in sync," never recurse, and **silently drop the divergent rows** —
reintroducing the exact hole this spec removes. A collision at a *high* range is a
missed diff, not a delay.

**Chosen fingerprint (prefer A):**
- **Option A (preferred) — port the proven primitive.** Lift the `Fingerprint`
  and range-split logic from willow-rs / iroh-docs' RBSR (Aljoscha Meyer,
  "Range-Based Set Reconciliation," which Willow implements): a cryptographic
  incremental set hash, not XOR, soundness already argued. ASP feeds it the `id`
  set; we do **not** adopt iroh-docs' KV data model. This is the research's
  "adopt the primitive, don't re-derive it" recommendation scoped to the
  reconciler.
- **Option B — implement fresh.** Fingerprint = a multiset hash: combine `H(id)`
  under a group op with negligible collision probability (addition in a large
  prime field, or EC-point summation). NOT XOR. Ships with the
  collision-adversarial fuzz test (§9), mandatory on this route.

**Store-side (v1).** Register a SQLite aggregate (rusqlite
`create_aggregate_function`) implementing the combiner: `SELECT fp(id) FROM log
WHERE id>=?1 AND id<?2` — the PK index bounds the scan. **Optimization (post-v1):**
a Merkle-range/Fenwick tree keyed by id, updated on append, for O(log n) range
fingerprints; defer until profiling justifies it.

### 2.2 Wire protocol (capability-gated, no PROTO bump)

Extend `Hello` with an optional, defaulted capability list:

```rust
Hello {
    proto: u32,            // stays 4
    node_id: String, vault_id: String, is_listener: bool,
    #[serde(default)] auth_key: Option<String>,
    #[serde(default)] caps: Vec<String>,   // NEW; "rbsr" advertises support
}
```

`caps` is `#[serde(default)]`, so a pre-RBSR proto-4 peer omits it and
deserializes fine. **Both** sides must advertise `"rbsr"` to use it; otherwise the
exchange falls back to the existing `Msg::Vector` path verbatim. **No `PROTO` bump
is required** (§7): existing `Msg` variants serialize byte-identically, and an
un-upgraded peer never *receives* a `Reconcile` frame (we send it only after
seeing `"rbsr"` in its `Hello.caps`), so its rmp_serde enum decoder never meets an
unknown variant.

```rust
enum Msg {
    // … existing variants unchanged …
    Reconcile { parts: Vec<RangePart> },   // NEW, gated behind caps="rbsr"
}
struct Bound(Option<String>);   // None = open end (−∞ for lo, +∞ for hi)
enum RangePart {
    /// "My fingerprint for [lo,hi). Match ⇒ range done; differ ⇒ split & reply."
    Fingerprint { lo: Bound, hi: Bound, fp: Fingerprint },
    /// Leaf: range small enough to enumerate. Reply with the ids you hold that I
    /// lack (as rows); `want` asks the peer to send its own leaf back.
    ItemSet { lo: Bound, hi: Bound, ids: Vec<String>, want: bool },
}
```

Actual row payloads still travel as today's `Msg::Rows { rows: Vec<WireRow> }`
(reusing byte-budget chunking + blob-dedup, §2.4), sent in response to an
`ItemSet`. `Msg::Synced` terminates the reconcile as now.

### 2.3 Session state machine (`session.rs`)

RBSR is **multi-round** where VV is one-shot, so `Session` gains reconcile state.
New `SessionVault` methods (both `Engine` and `MemEngine`):

```rust
fn caps(&self) -> Vec<String>;                       // ["rbsr", …]
fn fingerprint(&self, lo: &Bound, hi: &Bound) -> AspResult<Fingerprint>;
fn item_ids(&self, lo: &Bound, hi: &Bound) -> AspResult<Vec<String>>;
fn rows_by_ids(&self, ids: &[String]) -> AspResult<Vec<WireRow>>;
// version_vector / rows_after_wire retained for the fallback path.
```

**Flow (symmetric; either may open):**
1. After auth, if `"rbsr" ∈ peer.caps ∩ our caps`, the initiator sends
   `Reconcile { parts: [Fingerprint{ −∞, +∞, fingerprint(−∞,+∞) }] }` instead of
   `Msg::Vector`. Otherwise it sends `Msg::Vector` (unchanged §1).
2. On inbound `Fingerprint{lo,hi,fp}`: compute local `fingerprint(lo,hi)`.
   **equal** → range converged, emit nothing. **differ, item-count > `RBSR_LEAF`**
   → split `[lo,hi)` into `RBSR_SPLIT` sub-ranges by **item count** (median-id
   split via the PK index, balanced regardless of clustering), reply a
   `Fingerprint` per sub-range. **differ, small** → reply
   `ItemSet{lo,hi,ids:item_ids(lo,hi),want:true}`.
3. On inbound `ItemSet{lo,hi,ids,want}`: ids the peer holds that we lack → ask via
   our own `ItemSet` (if `want`); ids we hold that the peer lacks → ship as
   `Msg::Rows` (chunked). When no range remains unconverged, send `Msg::Synced`.
4. `Msg::Rows`/`Msg::Push` integrate exactly as today (`integrate_many`,
   session.rs:311) — RBSR changes only *which* rows are selected, never how they
   fold. Split parameters `RBSR_LEAF` (≈32) and `RBSR_SPLIT` (≈16) tune with §9.

**Determinism.** Median-id splitting over the PK index is a pure function of the
held id-set, so two nodes derive the same range tree for the same sets — minimal
exchange, reproducible in tests.

### 2.4 Drivers and payload reuse

- `stream_catchup` (iroh_net.rs:400) — today's one-shot "for each site, page
  `rows_after_wire` and dump" — is replaced, **for rbsr peers only**, by a pump
  that feeds inbound `Reconcile` frames through `session.on_msg` and sends the
  emitted `Fingerprint`/`ItemSet`/`Rows` steps. Non-rbsr peers keep the existing
  `Step::CatchUp` → `stream_catchup` path untouched.
- **Reuse, don't re-solve, the payload layer.** Final `Msg::Rows` responses go
  through the existing `push_rows_chunked` byte budget (session.rs:129) and the
  whole-catch-up **blob dedup** (`sent_blobs`, iroh_net.rs:406-421), correct
  because they operate on the row/blob stream regardless of id selection.
- `iroh_wasm.rs` mirrors the pump (its `Msg::Vector` handling at :278). A browser
  node is a **connector**, and RBSR's connector side is a bounded
  request/response exchange — a natural fit for `feed()`.
- **Periodic anti-entropy** becomes a periodic top-level `Fingerprint` send for
  rbsr peers: cheaper than a full VV recompute, and it *closes holes* rather than
  re-advertising a watermark that may be a lie.

### 2.5 How RBSR relaxes the partial-sync constraints

A's v1 correctness rests entirely on the **pull-only-leaf** constraint (§3.1,
§10 risk 1): a filtered replica's sparse `MAX(seq)` VV would hand a third peer a
permanent hole, so it may never serve or add a second upstream. **RBSR removes the
root cause** — reconciliation is over actual id membership, so a filtered replica
with a sparse row-set reconciles correctly against *any* peer without lying about
completeness. Therefore, once RBSR ships:
- A's multi-upstream / non-leaf form (the partial-sync **Phase 4**, previously
  gated on a `PROTO`-5 "per-scope frontier on the wire") is unlocked **without**
  that wire change — RBSR is the better answer to the same problem.
- The scoped send-filter (§3.2) still applies on the *listener* side; RBSR governs
  *completeness accounting*, the filter governs *what is in scope*. A scoped
  replica advertises fingerprints only over its retained id-ranges.

Build order: land RBSR (§8 Phase R) before attempting non-leaf A. The leaf-only A
(Phases 1–2) can ship on VV first and gain the general form for free once RBSR
lands.

---

## 3. Feature A — Partial subdir sync

### 3.1 Chosen approach

**A single-subdir (v1) or path-prefix clone, enforced as a listener-side SEND
filter, with the grant stored server-side in `authorized_keys` and keyed by the
peer's transport-verified `node_id`.** The scoped node is a *dumb connector*: it
sends its normal `Hello` and VV and receives what the listener chose to send —
**no `Hello` field, no new `Msg` variant, no `PROTO` bump**.

The filter operates on **whole `file_id` chains**, never per-row path, resolved
from the listener's own fold — the only way to satisfy both fold-completeness
(never split a Create from its descendants) and `resolve_paths` correctness (a
complete path-prefix cut).

Three hard invariants make the dense-seq model correct **without RBSR** (with
RBSR, invariant 1 relaxes — §2.5):
1. **The scoped replica is a pull-only LEAF pinned to exactly ONE upstream.** It
   never serves catch-up to a third peer and never adds a second upstream, so its
   sparse `MAX(seq)` VV is never advertised as authoritative. Enforced by a local
   `partial` flag: (a) refuse `Listener` role / `watch --listen`, (b) refuse a
   second peer, (c) label the UI "partial."
2. **Membership is monotonic "ever resolved under X"** (§3.3) so the send set
   never *un-sends* a file — no tombstone synthesis, no forged local deletes.
3. **Scope-widening is an explicit re-clone**, not a live backfill (§3.2).

### 3.2 The VV / dense-seq solution (leaf-only path)

At a **fixed scope with one upstream**, the existing `MAX(seq)` model is already
correct, because of the cursor-before-filter ordering (§1):
- `stream_catchup` advances its paging `cursor` from the *unfiltered*
  `page.last().row.seq` at **iroh_net.rs:417**, so the examined frontier moves
  across dropped seqs while the shipped set stays sparse.
- On reconnect the scoped replica advertises `version_vector() = MAX(held)` and
  asks `seq > MAX(held)`; the listener re-runs the *same deterministic* filter
  over the tail. Out-of-scope rows are never delivered — no re-request loop.

**The filter MUST be inserted between line 417 and line 419** — after the cursor
advance, **before** `sent_blobs.retain` — because each `WireRow` self-carries its
`base_hash`/`result_hash` blobs (wire.rs:34-43) and the dedup marks a content hash
"sent" on first sight. Filtering *after* the dedup would let a dropped
out-of-scope row mark a *shared* blob sent, so a later in-scope row referencing
that hash ships blob-less → the receiver folds `unwrap_or_default()` empty bytes
(fold.rs:117-122) and silently diverges.

```rust
// iroh_net.rs stream_catchup, between :417 and :419
cursor = page.last().map(|w| w.row.seq as i64).unwrap_or(cursor);   // :417 (unchanged)
page.retain(|wr| scope.admits_row(wr, &members));                    // NEW: whole-chain membership
for wr in &mut page { wr.blobs.retain(|b| sent_blobs.insert(b.hash.clone())); } // :419-421 (now over survivors)
```

**Scope-widening cannot backfill via the VV** — now-in-scope rows sit *below* the
receiver's `MAX(seq)` and are unrequestable (`rows_after` is strictly `seq >
peer_vv`). Handle as an admin action = **re-clone at the wider scope** (or reset
the replica's advertised vector for affected sites; `append_rows`
INSERT-OR-IGNORE dedupes, sqlite.rs:259). Named limitation in `--subdir` help.
*(RBSR removes this limitation — a widened scope simply reconciles the
newly-in-scope ids; §2.5.)*

### 3.3 Rename-across-boundary handling

Two membership definitions, both needed:
- **SYNC membership (which rows to ship) = "the file_id EVER resolved under X."**
  Computed on the listener by scanning that file's `Create`/`Rename` rows
  (`rows_for_file`, sqlite.rs:329) through `Scope::allowed` (§3.6). Monotonic:
  - *Rename INTO X*: member via the in-scope Rename ⇒ its **whole chain ships
    including the out-of-scope Create** ⇒ fold materializes it (no orphan at
    fold.rs:164).
  - *Rename OUT of X*: still a member (monotonic) ⇒ the replica **receives the
    Rename-out row and learns the file left** — no stale ghost, no cross-boundary
    ghost-edit.
- **VIEW membership (what the replica displays) = "current folded path under X,"**
  applied as a **materialize-time filter mirroring branch visibility**
  (`vis.sees` at engine.rs:285). A file that has left X stays in the log but is
  hidden from disk/UI.

**Membership must include deleted tombstones.** `file_ids_under(prefix)` must
query the raw `files` table (tombstones persisted with path, fold.rs:286-297)
**without** a `deleted=0` clause — do **not** reuse
`live_files`/`live_file_by_path`/`file_id_for_path` (all filter `deleted=0`), or
an in-scope Delete's file_id drops out, the Delete never ships, and the receiver
keeps a live ghost.

**Realtime rename-into-scope (the subtle bug).** A lone `Msg::Push` for a Rename
that brings a file into scope arrives without its below-watermark Create → `else
continue` → the file vanishes forever. Fix in scope-aware fanout (§3.5): for a
**`Rename`** row that makes a file a member of a conn's scope, the listener ships
that file's **whole chain via `rows_for_file` as a `Msg::Rows` batch**, not the
lone `Push`. Idempotent (INSERT-OR-IGNORE), so also safe for rename-within-scope.
`Create`/`Edit`/`Delete`/`Reclass` for an already-member file ship as normal lone
`Push`es.

### 3.4 Non-file rows and causal ancestors

**Ship all non-file rows wholesale, never path-filter them.**
`Branch`/`Tag`/`Merge`/`GitCommit`/`GitIngest`/`GitPlan` are fold no-ops
(fold.rs:196-201) but their `path` field is overloaded, so a prefix test
misclassifies them, and `Branch` records are load-bearing for `fork_vv`
visibility (engine.rs:466/503). They are few and carry real seqs, so shipping all
keeps the examined-frontier cursor coherent. Causal ancestors of in-scope files
are covered for free by whole-`file_id`-chain shipping.

**Git-bridge hazard — must close.** A partial fold synthesizes a partial *root*
tree (`build_tree_object` with `prefix=""` over the whole fold), which on push
**deletes every out-of-scope file on the remote**. The real danger is **`GitPlan`
authorship**: a plan authored on a partial replica syncs to full nodes that then
push the partial tree. So a `partial` replica must **hard-disable git push AND
suppress the interval auto-plan policy** (mirror the browser push-disable + the
per-remote `frozen` gate). Falls out of the pull-only-leaf flag.

### 3.5 Hook points (fn + file:line)

| Seam | Location | Change |
|---|---|---|
| **Primary catch-up filter** | `stream_catchup`, iroh_net.rs:400-432 | Insert `page.retain(scope)` between :417 and :419 (§3.2). Thread the conn's `PeerPolicy` via `Step::CatchUp`. |
| **Realtime filter + rename backfill** | `fanout`, net.rs:53-60 + callers iroh_net.rs:343 (`Step::Integrated`), net.rs (watcher) | Extend `Conns` value from `mpsc::UnboundedSender<Msg>` to `(sender, PeerPolicy)`; skip out-of-scope rows; for a member-making `Rename`, send `rows_for_file` as `Msg::Rows` (§3.3). Must live **in fanout**, not only catch-up (the hub re-forward at :343 is the leak path). |
| **Inline catch-up (connector push-back / in-process / wasm-served)** | `catchup_rows`, session.rs:116-126 | Apply the same whole-chain filter, or this path leaks. |
| **Retain the grant** | `Session` struct session.rs:79-97; admit call session.rs:250-258 | Add `policy: PeerPolicy` field; store the admitted grant instead of discarding it. Thread into `Step::CatchUp { peer_vv, policy }` (session.rs:72/284). |
| **Membership resolver** | new `Engine::file_ids_under(prefix)` over raw `files` (index `files_path`, sqlite.rs:51) + `SqliteStore::rows_for_file` (sqlite.rs:329) | No `deleted=0` filter (§3.3). |
| **Whitelist matcher** | new `Scope::allowed(rel) -> bool` beside `Scope::ignored` (scope.rs:68), delegating to `glob_match` | §3.6. |
| **Materialize-time view filter** | fold path, mirroring `vis.sees` at engine.rs:285 | Hide files whose current path left X. |

### 3.6 `Scope::allowed`

`Scope::ignored` is a denylist (scope.rs:68-80) with an un-negatable
`ALWAYS_IGNORE_DIRS` guard. A subdir allow-list is its complement; the `*` +
`!subdir/**` inversion has a bare-dir footgun (`!subdir/**` doesn't re-include the
`subdir` row itself). Add a first-class `Scope::allowed(rel) -> bool` sibling
delegating to the memoized, DoS-hardened `glob_match` (the `failed` HashSet
already defends against a hostile synced `.aspignore`). `scope.rs` is
always-compiled, std-only, so the same predicate runs in wasm.

### 3.7 Multi-surface plumbing

- **CLI:** `asp authorize <pubkey> --subdir PATH` stores the grant; `asp clone
  --subdir PATH` records only the local `partial` leaf flag.
- **Desktop:** one new Tauri command via the f6c1d07 5-file ceremony
  (DesktopEngine method; `commands.rs` pass-through; `generate_handler!`
  registration; `api.ts` interface + both impls; extend the invoke-arg guard
  `desktop/src/lib/tauriApi.git.test.ts`). DTOs `#[serde(rename_all="camelCase")]`.
- **wasm:** mirror the whole-chain filter in the `MemEngine` catch-up path so a
  browser node can never become the laxer leak; `Scope::allowed` already compiles
  to wasm.

**Effort: L** (single-subdir, monotonic membership, star leaf, no PROTO bump). XL
for general whitelisting + strict current-scope membership + multi-upstream — the
last of which RBSR (§2.5), not a wire frontier, is the intended enabler.

---

## 4. Feature B — Read-only subdirs

### 4.1 Chosen approach and the honest trust model

B = A's read-scoped subdir clone + a per-peer **write refusal**, whose *strength*
is set by the vault's **security profile** (§4.4). Three honestly-distinct levels:

1. **Advisory (mesh peers, Verified-mode rules NOT enforced) — NOT a real
   boundary, never ship this.** A "read-only" peer pushes to a laxer node B, B
   integrates and fans out (iroh_net.rs:339-345), and the restricting node A pulls
   it back via anti-entropy (session.rs:283-286). A cannot even tell the row came
   from the blocked peer: rows carry the original author's `site_id` and
   `integrate_many` checks only `id_valid()` (engine.rs:482), which proves
   hash↔fields, **not authorship**. This is the downgrade trap (§4.4). Only ever
   surface as best-effort, never as a boundary.
2. **Trust mode (star, no signatures) — a real boundary via topology, and the
   default.** The read-only peer connects *only* to the source; the source is the
   single integrator and rejects the peer's inbound rows using the QUIC-verified
   `peer_node` (session.rs:94/245). Zero crypto cost. Same topology as C and the
   single-user hub. The guarantee is a *deployment* constraint (§10 risk 2).
3. **Verified mode (mesh, mandatory signatures) — cryptographic, opt-in.** ed25519
   sigs mandatory *within this vault* (sign at every builder, `verify_detached` +
   author→path check at `integrate`, incl. wasm `MemEngine`). Read-only — and A's
   subdir-read confidentiality — then hold regardless of topology. A
   **user-selectable per-vault mode** (§4.4), **not** a `PROTO` bump.

### 4.2 Enforcement point (fn + file:line)

Gate `integrate_batch` on the retained `PeerPolicy` **before** it calls
`vault.integrate_many` — in the `Msg::Rows` arm (session.rs:289-295) and
`Msg::Push` arm (:296-302):

```rust
// session.rs Msg::Rows / Msg::Push, before integrate_batch (:311)
if self.policy.read_only && rows.iter().any(|wr| is_file_mutation(&wr.row)) {
    return Ok(vec![Step::Send(Msg::Denied { reason: "read-only".into() }),
                   Step::Integrated(vec![])]); // empty so the connector's own catch-up still completes
}
```

`is_file_mutation` = `kind ∈ {Create,Edit,Rename,Delete,Reclass}`. Lives in the
**sans-IO `Session`**, so native `Engine` and wasm `MemEngine` enforce identically
— mandatory, or the browser becomes the laxer node. No `integrate_many` signature
change. Path-granular read-only is B-plus; whole-connection read-only is v1. The
deeper choke for regime 3 is `Engine::integrate`/`integrate_many`
(engine.rs:452/480) where `peer: &NodeId` + `verify_detached` + author→path check
would be added.

### 4.3 Phase-1 shippable

**Trust mode is phase-1:** reuse A's `authorized_keys.read_only` column +
`PeerPolicy` retention; add the ~one-branch reject in `on_msg` + the `MemEngine`
mirror + a CLI `--read-only` on `authorize`. No signatures, no engine-trait
change, no PROTO bump. **Verified mode (§4.4)** layers signing on top later; it
does not block phase 1. **Effort: S–M** (Trust); **L** (Verified, §4.4).

### 4.4 Security profiles: Trust vs Verified (optional signing)

Signing is a **per-vault mode the user chooses**, made clean by one frozen fact:
**`sig` is excluded from the Merkle id** (`canonical_fields()` omits it; the
`sig_does_not_affect_id` test pins it, log.rs:183/346). A signed row and the same
row unsigned have the **identical id**, so signing is additive metadata — turning
it on/off never changes ids, never breaks content-addressing or dedup, and
**never forks a vault**.

| | **Trust mode** (default = today) | **Verified mode** (opt-in) |
|---|---|---|
| Author signs | no (`sig: vec![]`) | yes, every mutating row |
| Integrate check | `id_valid()` only (engine.rs:482) | `id_valid()` + `verify_detached` + author→path ACL; unsigned/wrong-author → rejected |
| Enforcement basis | topological (single integrator) + connection read-only | cryptographic — travels with the data |
| Safe topology | star only | true P2P mesh |
| Crypto cost | zero | one-time verify on clone (below) |

**It MUST stay a per-vault mode, never per-row "signed or not, both accepted" —
that is the downgrade attack.** If any node accepts unsigned rows, an attacker
strips the `sig` and re-sends; the unsigned copy launders through the lenient node
back to strict nodes via anti-entropy. Verified-mode nodes **reject**
unsigned/unauthorized rows fleet-wide.

**The mode lives at genesis, inherited by every clone.** A local config flag is
insufficient (a fresh clone would accept unsigned rows — silent downgrade). Bake
the profile into the **vault genesis / manifest** so every replica learns it on
clone. At the connection layer, advertise the profile in `Hello` as an
**additive** `#[serde(default)]` field so a Verified node refuses/warns on a peer
that would feed it unsigned rows. **No forced `PROTO` bump**: Trust is
byte-identical to today; Verified populates an additive wire field and enforces
locally.

**Enforce at integrate, not at fold.** Both the signature verify and the
author→path ACL run once, at row entry (`integrate_many`, engine.rs:482), so the
stored log is "already trusted" and every subsequent fold / `state_as_of` /
`file_at` pays **zero** crypto.

**Performance envelope (Verified).** Sign once per *your* edit (~15–20 µs, dwarfed
by the SQLite write already in `record_write`). Verify once per row per node on
first receipt, never repeated: a cold clone of 100k changes ≈ 4 s single-threaded,
≈ 1.5 s with `ed25519-dalek::verify_batch` per `CATCHUP_PAGE_ROWS` page, ≈ 0.4 s
batched across cores — overlapping the network transfer. Storage/wire: +64
bytes/row (~6.4 MB for 100k).

**Turning an existing Trust vault into Verified** is the one hard part: historical
rows are unsigned and a strict node would reject its own history. Resolve with a
**signing epoch** — grandfather every row before a cutoff frontier as trusted,
require signatures only after — the same "pre-migration grace" the repo uses
(`authkeys.rs` admits `expires_at IS NULL`; pre-branching rows default to `main`).
Verified-at-genesis avoids it. Verified → Trust is a deliberate downgrade — gate
behind an explicit, logged admin action.

**wasm parity is mandatory:** `MemEngine` integrate must sign/verify identically,
or the browser becomes the lenient node. *Deferred — per-subdir profile:* feasible
on A's path scoping but the verify decision becomes path-dependent; ship
whole-vault mode first.

---

## 5. Feature C — Thin remote-view client

### 5.1 Chosen approach

A source node exposes read/query + write + subscribe over a **separate iroh ALPN
(`asp/query/1`)**, alongside — and independent of — the row-streaming sync ALPN.
The thin client keeps **no local log or blobs**: every read is a *server-side
fold*, every write is *authored by the source*. A separate ALPN leaves
`Msg`/`PROTO` untouched (no bump) and makes thin-view an opt-in server capability.

**Why ALPN over an HTTP sidecar:** the source's authorization is keyed by iroh
`node_id` (`authorized_keys`), and the QUIC handshake already proves the client's
`node_id` (session.rs:245). So the query ALPN **reuses A's `allowed_paths` and B's
`read_only` directly, with no separate bearer-token→policy table** and no
signatures. It also traverses the existing relay, so a browser wasm client can
reach it. *(A plain-HTTP gateway for non-iroh clients is a later optional
deployment, modeled on the vendored `hyper` stack at gitproxy.rs:67-73 + a
`node_id`/token→policy bridge.)*

The star is **naturally enforced**: a thin client speaks *only* the query ALPN,
never the sync ALPN, so it is not a sync participant — no client-to-client row
path to disable, no `fanout` carve-out to get wrong.

### 5.2 Read/query + subscribe protocol

New frame types in their own module (not `wire.rs::Msg`), dispatched by a new
`ThinSession` handler parallel to `Session::on_msg` (session.rs:195), driven from
a new accept loop beside `serve` (iroh_net.rs):

- `Query{ id, ListDir{path} | ReadFile{path} | ReadFileAt{path,ts} | Stat{path} }`
  → `QueryResp{ id, .. }`, answered from the source's full store: HEAD via
  `live_files` (sqlite.rs:649, indexed); as-of via `state_as_of`/`file_at`
  (engine.rs:1344/1367 — O(whole log), acceptable for occasional history-slider
  use). Every result filtered by the client's `allowed_paths` grant (A).
- `Submit{ id, Write{path,bytes} | Rename{from,to} | Delete{path}, envelope_sig,
  nonce }` → `SubmitResp{ id, result }` (§5.3).
- `Subscribe{ sub_id, path_prefix }` / `Event{ sub_id }` / `Unsubscribe{ sub_id }`
  — **signal-then-pull** for v1 (§5.4).

### 5.3 Write-through, authorship, attribution, causal validity

The client **cannot** author under its own `site_id`: `record_write` hardcodes
`site_id: self.site_id()` + a dense `next_seq` (engine.rs:313/292), and two
writers on one `site_id` collide on `UNIQUE(site_id,seq)` (sqlite.rs:28) and
defeat VV catch-up. And `canonical_fields()` is frozen — no `authored_by` field
without forking every vault.

So **the source authors the row on the client's behalf** via
`Engine::record_write`/`record_remove`/`record_rename` (engine.rs:298/369/397);
the row is causally valid, convergent, fans out to full peers normally, and
history legitimately says *"the source authored it."* Per-user attribution rides
**outside the row**:
- The client signs the `Submit` **envelope** (`path + bytes + nonce`) with its
  ed25519 identity; the source verifies with `verify_detached` (identity.rs:81)
  against the client's `authorized_keys` `node_id` **before** authoring — the
  first real use of the inert verify path, scoped to thin-client submits (no
  PROTO bump; the row's own `sig` stays empty in Trust mode — in Verified mode the
  source signs the row with its **own** key, §4.4).
- The source records `(row_id → client_node_id, envelope_sig, ts)` in a
  **node-local, never-synced** `remote_edits` table (§6). Honest limitation: other
  replicas and the derived git author line see only "source authored"; crypto
  attribution is source-local.

**`.aspignore` no-op trap:** `record_write` returns `Ok(None)` for an ignored path
(engine.rs:299-301). The `Submit` handler must detect `None` and return an
explicit error, not silent success.

**Lost-update trap (mitigated):** `record_write` builds a *linear* Edit against
the source's current tip, so two thin clients on one path serialize as source-side
LWW. Mitigation: the client sends the `base_hash` it read; the source rejects with
a conflict if the tip moved (optimistic concurrency).

### 5.4 Live updates

v1 = **signal-then-pull**, mirroring the desktop `vault-changed` pattern: hook
`Engine::set_change_listener`/`notify_change` (engine.rs:442/446, fired in
`integrate_many` :518) to emit a bare `Event{sub_id}` to each subscriber whose
scope intersects the change; the client re-queries the affected subtree via
`live_files` (indexed). Path-level `Delta{path→bytes|tombstone}` frames are a
later optimization.

### 5.5 Offline / latency tradeoff

C **abandons offline and local-first**: no local log ⇒ no offline read/write,
every read is a round-trip, reads are source-authoritative, and the source is a
SPOF. This is the deliberate enterprise "one single-source-of-truth node" trade —
full/partial replicas remain the offline path. Optional mitigation: a small LRU of
recently-read blobs/state for **read-only** offline viewing (never authoring).

### 5.6 Hooks and surface order

Reads: `state_as_of`/`file_at`/`live_files` (engine.rs:1344/1367, sqlite.rs:649).
Writes: `record_write`/`record_remove`/`record_rename` (engine.rs:298/369/397).
Subscribe: `set_change_listener`/`notify_change` (engine.rs:442/446). Auth:
`authorized_keys` `allowed_paths`/`read_only` (A/B). Attribution:
`verify_detached` (identity.rs:81) + new `remote_edits` table.

**Surface order:** build the **source** first (`asp serve` opening `asp/query/1`
on a native node), smoke-test with a CLI `asp view <ticket> --paths` client, then
add the **web thin-client backend** (an alternate `api.ts` impl targeting the
query ALPN over the relay). **Desktop stays a full replica** (it wants offline);
the web app is the natural first thin client. **Effort: L–XL.**

---

## 6. Data model / schema changes

All new state is **node-local, never synced** (except the genesis security
profile, §4.4), added with the house convention (append to `SCHEMA` for fresh DBs
+ guarded `ALTER` for existing; no version table).

**`Hello.caps` (RBSR, §2.2)** is a wire field, not schema — additive
`#[serde(default)]`, no migration.

**`authorized_keys` gains two columns (A + B).** Add to the `CREATE TABLE` in
`SCHEMA` (sqlite.rs:60-63) for fresh DBs, and a new `migrate_authz()` modeled on
`migrate_branching` (sqlite.rs:157-182) / `migrate_git_push`: read `PRAGMA
table_info(authorized_keys)` into a `HashSet`, then guarded `ALTER TABLE ... ADD
COLUMN` only if absent. Wire into `init` at sqlite.rs:147-148.

```sql
ALTER TABLE authorized_keys ADD COLUMN allowed_paths TEXT;              -- glob JSON; NULL = full
ALTER TABLE authorized_keys ADD COLUMN read_only INTEGER NOT NULL DEFAULT 0;
```

Extend `AuthKey` (authkeys.rs:64-77), `authkey_from`/`insert_authkey`, and
`engine.authorize(..)`. Surface the grant through `AdmitCtx`/`decide_admission`
and **retain it on the `Session`** (fix the discard at session.rs:251).

**`remote_edits` (C):** node-local attribution side table:

```sql
CREATE TABLE IF NOT EXISTS remote_edits(
  row_id TEXT PRIMARY KEY, client_node_id TEXT, envelope_sig BLOB, submitted_at INTEGER);
```

**Security profile (§4.4) is a genesis property, not a synced table row.** Store
`Trust | Verified` — and any Trust→Verified signing-epoch cutoff (a
lamport/frontier watermark) — in the vault manifest / genesis record so it is
inherited by every clone and cannot be locally downgraded. Read on the integrate
path to decide whether to verify. A node-local mirror may follow the house
convention (`CREATE TABLE IF NOT EXISTS signing_epoch(...)`), but the
authoritative copy is the inherited genesis property.

**Do not repurpose the dead `peer_state` table (sqlite.rs:58)** — RBSR makes a
per-`(peer,scope)` receiver cursor unnecessary; use fresh, purpose-named tables if
any local reconcile bookkeeping is ever needed.

---

## 7. Frozen-rule & PROTO impact

**`canonical_fields()` / `oid::merkle_id` do NOT change and must NOT**
(log.rs:219-235). RBSR reconciles the *set* of rows (never touches a row); A/B are
pure whole-row *selection* — never rewrite `path` (field 10) or renumber `seq`
(field 2); C authors *normal* sealed rows. Populating `sig` is safe (excluded from
`canonical_fields`, log.rs:183) — Trust leaves it empty, Verified fills it yet
still does not fork. **No vault forks.** This holds under inspection.

**`PROTO` stays 4** for everything in this spec:
- **RBSR:** gated behind the additive `Hello.caps`; an un-upgraded peer never
  receives a `Reconcile` frame and falls back to `Msg::Vector` (§2.2). No bump.
- **A:** scope is server-granted in `authorized_keys`, enforced by the listener;
  the connector's wire behavior and `Hello` are unchanged. No bump.
- **B (star):** an integrate-time reject + node-local schema. No bump.
- **C:** a **separate ALPN** (`asp/query/1`) leaves the sync `Msg`/`PROTO`
  untouched. No bump.
- **Verified mode (§4.4):** an additive `#[serde(default)]` `Hello` mode advert;
  `sig` already exists on the wire. An old peer is simply refused admission to a
  Verified vault, gracefully. No bump.

**`PROTO` → 5 is required only for one deferred variant that this spec explicitly
supersedes:** client-*requested* scope with per-scope frontiers on the wire (the
old multi-upstream partial-sync plan). **RBSR (§2.5) is the intended replacement**
— it delivers multi-upstream partial sync with no wire frontier and no hard
compatibility break, so Phase 4's `PROTO` 5 should be considered obsolete in
favor of Phase R + non-leaf A.

---

## 8. Phased roadmap

**Phase R — RBSR anti-entropy (M–L, foundational, parallelizable).** The
fingerprint (§2.1, prefer porting willow-rs/iroh) + the SQLite range aggregate +
the `SessionVault` fingerprint/item/rows-by-id methods + the `Hello.caps`
negotiation + the `Msg::Reconcile` variant + the Session reconcile states + the
driver pump (native + wasm) + collision/scaling/parity tests (§9). Keep VV as the
negotiated fallback; flip `caps=["rbsr"]` on by default once the fallback e2e is
green. **No `PROTO` bump.** *Unblocks the general (non-leaf, multi-upstream) form
of A (§2.5) and closes the silent-hole hazard fleet-wide.*

**Phase 0 — Policy plumbing (S).** `migrate_authz()` + `authorized_keys.{allowed_paths,
read_only}` (§6); extend `AuthKey`/`authkey_from`/`insert_authkey`/`engine.authorize`;
retain the admitted `PeerPolicy` on the `Session` (fix session.rs:251). *Unblocks
A, B, and C's authorization.*

**Phase 1 — Read-only whole-peer (S–M).** B regime 2: the `on_msg` reject
(session.rs:289-302) + `MemEngine` mirror + `asp authorize --read-only` + parity
test. *Ships one-way sync from a hub today; no scoping yet.*

**Phase 2 — Single-subdir clone (L).** A's whole-`file_id`-chain filter in
`stream_catchup` (between iroh_net.rs:417 and :419) + `catchup_rows`
(session.rs:116) + scope-aware `fanout` with rename-into-scope whole-chain reship
(net.rs:53) + `Scope::allowed` + `file_ids_under` (raw `files`, tombstones kept) +
the materialize-time view filter + the `partial` pull-only-leaf flag (refuse
listener/second-upstream, suppress git push + GitPlan). CLI `--subdir` + one
desktop command + wasm parity. **Phase 1 + Phase 2 together = "read-only
single-subdir clone from a hub"** — the recommended first deliverable. *With Phase
R landed, the pull-only-leaf constraint relaxes (§2.5).*

**Phase 3 — Thin remote-view client (L–XL).** C: `asp/query/1` ALPN + `ThinSession`
+ query/submit/subscribe frames; server-authored write-through with signed-envelope
attribution + `remote_edits`; signal-then-pull subscription; web thin-client
backend. *Reuses Phase 0's `allowed_paths`/`read_only` and Phase 2's fold-membership
resolver directly.*

**Phase 3.5 — Verified security profile (L, opt-in, any time after Phase 1).** The
mesh trust boundary as a **user-selectable per-vault mode** (§4.4): genesis-set
`Trust | Verified`; sign in `record_write`/`record_remove`/`record_rename`;
`verify_detached` + author→path ACL at `integrate_many` (engine.rs:482); the
additive `Hello` mode advert; `MemEngine` parity; batch-verify on clone. **No
`PROTO` bump.** The signing-epoch grace for a Trust→Verified migration is the extra
increment; Verified-at-genesis skips it. *Unblocks true P2P read-only +
subdir-read confidentiality.*

**Phase 4 (superseded) — Multi-upstream partial sync.** Formerly `PROTO`→5 with a
client-requested `Subscription` in `Hello` + per-`(peer,scope)` frontier cursor.
**Replaced by Phase R + non-leaf A** (§2.5, §7): RBSR delivers the same capability
without the wire frontier or the hard compatibility break.

---

## 9. Test strategy

Per `.claude/skills/verification-playbook`: house style is **deterministic LCG
fuzz inside ordinary `#[test]`s — no proptest, no cargo-fuzz**; hermetic
`tempfile::tempdir()` + `Identity::from_seed`; the three high-leverage patterns are
ground-truth invariant, byte-determinism, and N-vs-2N scaling.

**RBSR — convergence gate (ground-truth invariant).** After a reconcile between
two `MemEngine`s, assert **equal id-sets** *and* byte-identical fold state — the
property VV cannot guarantee under holes.

**RBSR — hole fuzz (headline).** LCG-seed two engines with random, deliberately
**gapped** per-site row-sets (`{0,1,2,5}` shapes, disjoint tails, interleaved
authors), reconcile, assert equal id-sets. A VV reconcile provably fails this;
RBSR passes every seed.

**RBSR — collision-adversarial fingerprint.** Construct two *different* in-range
id-sets and assert their fingerprints differ (guards against an XOR-class combiner
slipping in). Mandatory if Option B is taken.

**RBSR — N-vs-2N scaling.** A *synced* pair reconciles in **1 round / 0 rows
shipped**; a k-divergent pair ships ≈ k rows in ≈ `log(n)` rounds — assert
round-count grows sub-linearly.

**RBSR — byte-determinism + fallback.** `fingerprint(range)` identical native vs
wasm for the same id-set (SDK conformance vector). A `caps=["rbsr"]` node against a
peer advertising no `caps` negotiates down to `Msg::Vector` and still converges (a
pinned e2e process); no `Reconcile` frame is ever sent to it.

**A — ground-truth invariant (load-bearing).** A subdir-scoped replica's fold
**==** a full replica's fold **restricted to the in-scope file_ids**, byte-for-byte,
over a deterministic LCG history of create/edit/rename-across-boundary/delete.
Drive through the in-process two-session pump so it exercises the real
`stream_catchup`/`catchup_rows` filter.

**A — dense-seq regression.** Filter drops a mid-sequence slice ⇒ assert
convergence within scope and no forever-re-request (`{0,1,2,5}` with 3,4 out-of-scope
stays converged). Separately: rename-into-scope at **realtime** (a lone Push) ⇒
assert the whole chain reships and the file materializes (guards fold.rs:164).

**A — blob-dedup ordering + tombstone membership.** Two files sharing one content
blob, one in-scope; assert the in-scope file's bytes are correct (guards
iroh_net.rs:419). Delete an in-scope file; assert the Delete ships and the receiver
shows no ghost (guards the `deleted=0` helper trap).

**A — N-vs-2N scaling.** N in-scope + N out-of-scope files ⇒ the scoped clone
transfers ~N rows/blobs, not ~2N.

**B — negative test at the enforcing edge.** In-process pump: listener marks the
connector `read_only`; assert the connector's authored row **never** appears in the
listener's log, while the listener's rows **do** reach the connector. Mirror in
`MemEngine`.

**Verified mode — downgrade + enforcement.** In a Verified vault: (1) an unsigned
mutating row is **rejected** at `integrate_many` on every surface incl.
`MemEngine`; (2) a signed row from an author not authorized for that path is
rejected; (3) a signed authorized row converges; (4) byte-determinism — the same
content signed vs unsigned yields the **same Merkle id** (guards log.rs:346); (5)
epoch grace — pre-cutoff unsigned history accepted, post-cutoff unsigned rejected.
Exercise batch-verify via a ≥1-page clone.

**C.** `Submit` round-trips to exactly one source-authored `LogRow` (`site_id` =
source) + one `remote_edits` row; a bad envelope sig is rejected; an out-of-grant
`Query`/`Submit` refused; a subscribed client gets an `Event` after an unrelated
peer's in-subtree edit; an `.aspignore` path returns an explicit error (not
`Ok(None)`); the optimistic base_hash guard rejects a stale write.

**Flaky-e2e protocol.** Any networked-lane failure must be **baselined against a
clean worktree at HEAD** before attribution (the iroh/relay lane is flaky under VM
load regardless of the diff, per AGENTS.md); run the networked lane single-threaded
and rebuild `target/release/asp` first.

**Cross-surface soak.** After Phase 2, run the `sync-soak-test` harness (CLI `asp
watch --listen` + desktop engines + fuzzed file ops) with one scoped participant to
assert convergence + live UI update within scope and no out-of-scope leakage. After
Phase R, add an rbsr participant with an injected hole and assert it heals.

---

## 10. Risks & open questions (ranked)

1. **A's leaf-only correctness — removed by RBSR, present until then.** On the VV
   path the `MAX(seq)` model is correct *only* while a scoped replica pulls from
   one upstream and never serves; a missing `partial` flag or a second/laxer peer
   causes a leak or a permanent third-peer hole. Until Phase R lands, **this flag
   is the entire correctness guarantee** — enforce it structurally (refuse
   `Listener` and second upstream) and test it. **After Phase R, the hazard is
   gone** (§2.5) and multi-upstream is supported, not deferred.
2. **B's strength depends on the security profile (§4.4).** Trust mode is
   topological (a real boundary only in an enforced star; advisory in a mesh);
   Verified mode is cryptographic and holds in a true mesh. The UI must state the
   active mode — "read-only, enforced by this hub" vs "read-only, cryptographically
   enforced" — and never over-claim. The failure to avoid is **advisory**
   (risk 9).
3. **Realtime rename-into-scope is the subtle A bug.** A lone Push for a
   boundary-crossing Rename orphans the file unless the whole chain reships
   (§3.3). Verify with the realtime rename test.
4. **RBSR fingerprint soundness.** A weak (XOR-class) combiner silently drops
   divergent rows — the exact bug being fixed. Prefer porting the proven
   willow-rs/iroh primitive; the collision-adversarial test (§9) is the guard.
5. **Cross-surface parity is a doubled, easily-skewed surface.** Every RBSR method,
   A filter, and B reject must be mirrored in the wasm `MemEngine`, or the browser
   becomes the laxer node. Mandatory parity tests.
6. **A's confidentiality leaks cross-boundary path *names* under renames.** Because
   `path` is frozen and the fold needs the Create/Rename rows, a file that ever
   transited X ships a row bearing its out-of-X path. Subdir *read* scoping is a
   footprint/organization boundary and a star confidentiality boundary, **not** a
   guarantee that no out-of-subtree path string is ever visible.
7. **C abandons offline/local-first and makes the source a SPOF.** Inherent to "one
   single-source-of-truth node"; must be an explicit product decision, and desktop
   should remain a full replica.
8. **C attribution is source-local.** `remote_edits` is never synced and the row
   says "source authored." In-log per-user attribution is impossible without a
   vault fork (frozen `canonical_fields`); set expectations up front.
9. **C concurrent-write semantics.** Server-side LWW per path unless the optimistic
   base_hash guard is implemented; decide whether silent clobber or explicit
   conflict is the product behavior (§5.3).
10. **The downgrade attack is Verified mode's failure mode (§4.4).** If any node
    accepts unsigned rows, a stripped-signature copy launders through it fleet-wide.
    Signing must be a per-vault *mode* that **rejects** unsigned rows,
    genesis-inherited, `MemEngine`-enforced. The single thing to get right if
    Verified mode is built.
11. **Trust→Verified migration is the hard increment.** Historical rows are
    unsigned; without a signing-epoch grace a strict node rejects its own history.
    The epoch is idiomatic but must be chosen deliberately; Verified-at-genesis
    avoids it. Verified→Trust is a security downgrade — gate behind an explicit,
    logged action.

**Open questions for product before implementation:**
- Is single-subdir sufficient for v1 of A, or is a general path-set whitelist
  required at launch? (Single-subdir is L; general whitelist pushes toward XL — but
  RBSR removes the multi-upstream half of that cost, §2.5.)
- Default vaults to **Trust mode** (star, zero-crypto) with **Verified mode** as a
  per-vault opt-in (§4.4) — or is Verified the expected default for the target ICP?
  (Recommendation: Trust default; Verified opt-in at genesis, +L effort, no PROTO
  bump.)
- Land **Phase R (RBSR) before or after** the first read-only-subdir slice? (It is
  independent; landing it first makes A's general form free, but the leaf-only
  slice ships without it.)
- For C's first client: web thin-backend over the query ALPN (recommended), or a
  plain-HTTP gateway (needs a token→policy bridge but no wasm-iroh)?
