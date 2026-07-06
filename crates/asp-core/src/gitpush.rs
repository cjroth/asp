//! gitpush — the native **push** slice of the git bridge (git-bridge §5, M4):
//! deterministic commit synthesis from synced [`GitPlanRecord`] plans, in-memory
//! pack assembly, and the `asp git push` driver.
//!
//! Three layers, mirroring the read-path split:
//!
//! * **Plan authoring** ([`author_plan`], §5.3 `manual`): a plan is an ordinary
//!   synced `Kind::GitPlan` row saying "everything up to frontier F becomes one
//!   commit with message M". Authored under the LOCAL node's site/seq/lamport (it is
//!   a user action, not an imported git row). The policy slice adds *other* authors
//!   (`interval`/`llm`) without touching synthesis — the seam is this one function.
//! * **Deterministic synthesis** ([`synthesize_commits`], §5.2): a pure function of
//!   the fold + plans + ledger + base commit. Every input is synced/derived, so two
//!   bridge nodes compute byte-identical commit SHAs and object sets; racing pushes
//!   update the ref to the *same* sha. No leader election.
//! * **Push driver** ([`push`], §5.2/§9): base selection, `write_pack` +
//!   `push_pack`, idempotent-race handling, and the bounded non-fast-forward
//!   re-fetch/re-synthesize retry.
//!
//! Plus [`pending_git_diff`] (§5.3): the diff since the last plan/ingest frontier,
//! used to pre-fill push messages (and, later, the `interval`/`llm` policies).
//!
//! Native-only: it drives the on-disk [`Engine`] + [`RemoteStore`] + the async
//! transport. The tree/commit/blob encoding is the same git object model the read
//! path already decodes; here we run it forwards.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::branch::{version_vector_of, BranchSet, VersionVector, Visibility};
use crate::engine::Engine;
use crate::error::{AspError, AspResult};
use crate::gitbridge::{
    git_oid_bytes, push_pack, write_pack, GitBridgeError, GitObjectKind, GitRemoteSpec,
    RemoteStore,
};
use crate::gitrecord::{
    build_plan_row, decode_ingest_record, decode_plan_record, GitPlanRecord, GitRowIdentity,
};
use crate::log::{Kind, LogRow, MergeClass, MAIN_BRANCH_ID};
use crate::order::OrderKey;
use crate::sqlite::GitRemoteRow;
use crate::store::BlobStore;

/// Bounded non-fast-forward retries (git-bridge §9): a human pushing between our
/// fetch and push forces at most this many re-fetch/re-synthesize cycles.
const MAX_PUSH_ATTEMPTS: usize = 3;

/// The clone-seeded root ignore file (git-bridge §3.3). It is ASP-local control
/// state — materialized at clone time to keep `target/`/`node_modules/` out of the
/// synced log — and never existed in the upstream repo. Like `.asp/`, it must never
/// leave in a synthesized git tree, so we drop it from every pushed tree. (A repo's
/// real `.gitignore` is a *different* file and round-trips normally.) Root-only: a
/// nested `foo/.aspignore` is not something we seed, so we match the exact path.
const ROOT_ASPIGNORE: &str = ".aspignore";

/// The default commit identity when neither `--author` nor a vault-config author is
/// set (git-bridge §5.1). Matches the derived-export identity in `gitexport.rs`.
pub fn default_author() -> String {
    "asp <asp@asp>".to_string()
}

fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

// ===========================================================================
// Mode table (git-bridge §3.3 / R4) — the ledger's per-path mode cache
// ===========================================================================

/// The derived mode/symlink/gitlink cache (`git_modes`), consulted so a push
/// **replays the recorded git mode from the ledger, not the materialized form**
/// (git-bridge §5.2/R4 — the easy thing to get wrong, especially for a symlink a
/// web edit turned into a plain file). Load once per synthesis.
pub struct ModeTable {
    /// `path -> (git mode, kind)` where `kind` is `file` | `symlink` | `gitlink`.
    map: BTreeMap<String, (u32, String)>,
}

impl ModeTable {
    /// Load from the engine's `git_modes` cache.
    pub fn load(engine: &Engine) -> AspResult<ModeTable> {
        let mut map = BTreeMap::new();
        for (path, mode, kind) in engine.store.git_mode_get_all()? {
            map.insert(path, (mode, kind));
        }
        Ok(ModeTable { map })
    }

    /// The git tree-entry mode string for a *content* path (`100644` default,
    /// `100755` executable, `120000` symlink). Gitlinks are handled separately (they
    /// carry no fold content) — see [`synthesize_commits`].
    fn file_mode(&self, path: &str) -> &'static str {
        match self.map.get(path) {
            Some((_, kind)) if kind == "symlink" => "120000",
            Some((mode, _)) if *mode == 0o100755 => "100755",
            _ => "100644",
        }
    }
}

// ===========================================================================
// Plan authoring (git-bridge §5.3, `manual`)
// ===========================================================================

/// Author a [`GitPlanRecord`] as a synced `Kind::GitPlan` row (git-bridge §5.1/§5.3
/// `manual`). The plan's `frontier` is the current `main` version vector (the rows
/// the commit will include); it is authored under the **local** node's identity
/// (site/seq/lamport) because a plan is a user action, not an imported git row, and
/// must sync like any other row. Returns the sealed row so the caller can confirm it.
///
/// This is the whole policy seam: the later `interval`/`llm` slice authors plans the
/// same way (different trigger + message source), and synthesis stays untouched.
pub fn author_plan(
    engine: &Engine,
    remote_id: &str,
    message: &str,
    author: Option<&str>,
) -> AspResult<LogRow> {
    // Validate the remote exists so a stray push surfaces a clear error, not a plan
    // that can never be synthesized.
    if engine.store.git_remote_get(remote_id)?.is_none() {
        return Err(AspError::NotFound(format!(
            "no git remote configured (id {remote_id})"
        )));
    }
    let frontier = engine.visible_version_vector(MAIN_BRANCH_ID)?;
    let planned_ts = now_unix_secs();
    // Explicit `--author` wins; else the vault-wide `git.author` config; else the
    // derived-export default (git-bridge §5.1). The chosen string is recorded IN the
    // plan, so reading config here keeps synthesis deterministic.
    let author = author
        .map(str::to_string)
        .filter(|s| !s.trim().is_empty())
        .or_else(|| crate::config::VaultConfig::new(&engine.store).git_author().ok().flatten())
        .unwrap_or_else(default_author);
    let rec = GitPlanRecord {
        frontier,
        message: message.to_string(),
        author,
        planned_ts,
    };
    let lamport = engine.store.next_lamport(0)?;
    let seq = engine.store.next_seq(&engine.site_id())?;
    let ident = GitRowIdentity {
        site_id: engine.site_id(),
        lamport,
        seq,
        ts: planned_ts,
        parent: None,
    };
    let row = build_plan_row(&engine.store, &ident, &rec)?;
    engine.store.append_row(&row)?;
    Ok(row)
}

/// Every `Kind::GitPlan` record in the log, decoded and ordered the way synthesis
/// replays them (`(lamport, site_id, id)`). The order to hand [`synthesize_commits`].
pub fn plans_in_order(engine: &Engine) -> AspResult<Vec<GitPlanRecord>> {
    let all_rows = engine.store.all_rows()?;
    gather_plans(engine, &all_rows)
}

/// Gather every `Kind::GitPlan` record in the log, decoded and ordered by the same
/// `(lamport, site_id, id)` fold key rows converge on — the order synthesis replays.
fn gather_plans(engine: &Engine, all_rows: &[LogRow]) -> AspResult<Vec<GitPlanRecord>> {
    let mut plans: Vec<(OrderKey, GitPlanRecord)> = Vec::new();
    for r in all_rows {
        if r.kind != Kind::GitPlan {
            continue;
        }
        let Some(h) = &r.result_hash else { continue };
        let Some(bytes) = engine.store.get_blob(h)? else { continue };
        let Ok(rec) = decode_plan_record(&bytes) else { continue };
        let key = OrderKey {
            lamport: r.lamport,
            site_id: r.site_id.clone(),
            id: r.id.clone(),
        };
        plans.push((key, rec));
    }
    plans.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(plans.into_iter().map(|(_, r)| r).collect())
}

// ===========================================================================
// Deterministic synthesis (git-bridge §5.2)
// ===========================================================================

/// The output of [`synthesize_commits`] (git-bridge §5.2).
#[derive(Debug, Clone)]
pub struct SynthOutput {
    /// The base commit `B` the synthesized chain roots on (the real upstream tip our
    /// history is built on — its tree/blobs are already at the remote).
    pub base_sha: String,
    /// The tip of the synthesized chain (`== base_sha` when nothing is unpushed).
    pub tip_sha: String,
    /// The synthesized commit shas, in order (one per unpushed plan).
    pub commits: Vec<String>,
    /// Every NEW git object to send (blobs/trees/commits absent from the remote),
    /// ready for [`write_pack`].
    pub objects_to_push: Vec<(GitObjectKind, Vec<u8>)>,
    /// The effective frontier the tip represents — persisted as the push cursor so a
    /// later push knows which plans are already covered.
    pub pushed_frontier: VersionVector,
    /// Number of unpushed plans turned into commits.
    pub plans_pushed: usize,
}

/// Synthesize the git commits for the unpushed plans (git-bridge §5.2). **Pure over
/// synced/derived inputs**: identical rows + plans + ledger + base ⇒ byte-identical
/// commit SHAs and object set on any node, which is exactly what makes "any node may
/// push" safe without coordination.
///
/// `plans` must be ALL plans in fold order (the effective frontier is a running max
/// over earlier plans); `synthesize_commits` itself decides which are unpushed from
/// `remote.last_pushed_frontier`.
pub fn synthesize_commits(
    engine: &Engine,
    store: &RemoteStore,
    remote: &GitRemoteRow,
    plans: &[GitPlanRecord],
    modes: &ModeTable,
) -> AspResult<SynthOutput> {
    let all_rows = engine.store.all_rows()?;
    let bs = BranchSet::new(engine.store.branches()?);
    let vis = bs.visibility(MAIN_BRANCH_ID);

    // Base B = the real upstream commit our history is rooted on. Prefer our own last
    // push when it is ahead of (descends from) the ingest tip; else rebase onto the
    // ingest tip (upstream moved past our last push).
    let base_sha = choose_base(
        store,
        remote.last_pushed_sha.as_deref(),
        remote.last_ingested_sha.as_deref(),
    );

    // Ingest floor: the frontier of the last-ingested commit. Every synthesized tree
    // must include at least these rows, so a commit can never revert already-merged
    // upstream changes (git-bridge §5.1).
    let floor = ingest_floor(engine, &all_rows, remote.last_ingested_sha.as_deref())?;

    // Effective frontier per plan = running componentwise-max(floor, own, earlier).
    let mut acc = floor.clone();
    let mut effs: Vec<VersionVector> = Vec::with_capacity(plans.len());
    for p in plans {
        vv_max_into(&mut acc, &p.frontier);
        effs.push(acc.clone());
    }

    // Which plans are already pushed (their effective frontier is covered)?
    let pushed_frontier: VersionVector = remote
        .last_pushed_frontier
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    // Gitlink entries carried forward from the base tree (submodules have no fold
    // content, so re-inject them by their recorded commit sha, git-bridge §3.3).
    let gitlinks = base_sha
        .as_deref()
        .map(|b| collect_gitlinks(store, b))
        .unwrap_or_default();

    let mut objects: Vec<(GitObjectKind, Vec<u8>)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut commits: Vec<String> = Vec::new();
    let mut parent = base_sha.clone();
    let mut tip_frontier = pushed_frontier.clone();

    for (plan, eff) in plans.iter().zip(effs.iter()) {
        if covered(eff, &pushed_frontier) {
            continue; // already pushed
        }
        let mut files = fold_at_frontier(engine, &all_rows, &vis, eff)?;
        // §3.3: the clone-seeded root `.aspignore` is ASP-local and never existed
        // upstream — exclude it from the synthesized tree (see `ROOT_ASPIGNORE`).
        files.remove(ROOT_ASPIGNORE);
        let tree_oid = build_tree_object("", &files, &gitlinks, modes, engine, store, &mut objects, &mut seen)?;
        let body = commit_body(&hex::encode(tree_oid), parent.as_deref(), &plan.author, plan.planned_ts, &plan.message);
        let commit_oid = emit(GitObjectKind::Commit, body, store, &mut objects, &mut seen);
        let hexid = hex::encode(commit_oid);
        parent = Some(hexid.clone());
        commits.push(hexid);
        tip_frontier = eff.clone();
    }

    let tip_sha = parent.clone().unwrap_or_default();
    Ok(SynthOutput {
        base_sha: base_sha.unwrap_or_default(),
        tip_sha,
        plans_pushed: commits.len(),
        commits,
        objects_to_push: objects,
        pushed_frontier: tip_frontier,
    })
}

/// Choose the push base (git-bridge §5.2). `last_pushed` when it descends from the
/// ingest tip (we are ahead); otherwise the ingest tip (upstream advanced past us).
fn choose_base(store: &RemoteStore, last_pushed: Option<&str>, last_ingested: Option<&str>) -> Option<String> {
    match (last_pushed, last_ingested) {
        (Some(p), Some(i)) => {
            if p == i || store.is_ancestor(i, p).unwrap_or(false) {
                Some(p.to_string())
            } else {
                Some(i.to_string())
            }
        }
        (Some(p), None) => Some(p.to_string()),
        (None, Some(i)) => Some(i.to_string()),
        (None, None) => None,
    }
}

/// The frontier of the last-ingested commit (`{upto_site: upto_seq}` from its
/// `GitIngest` ledger record), the floor every synthesized tree must include.
fn ingest_floor(engine: &Engine, all_rows: &[LogRow], last_ingested: Option<&str>) -> AspResult<VersionVector> {
    let mut vv = VersionVector::new();
    let Some(sha) = last_ingested else { return Ok(vv) };
    for r in all_rows {
        if r.kind != Kind::GitIngest || r.path.as_deref() != Some(sha) {
            continue;
        }
        let Some(h) = &r.result_hash else { continue };
        let Some(bytes) = engine.store.get_blob(h)? else { continue };
        if let Ok(rec) = decode_ingest_record(&bytes) {
            let e = vv.entry(rec.upto_site).or_insert(-1);
            if rec.upto_seq as i64 > *e {
                *e = rec.upto_seq as i64;
            }
        }
    }
    Ok(vv)
}

/// Componentwise max of `other` into `acc`.
fn vv_max_into(acc: &mut VersionVector, other: &VersionVector) {
    for (s, q) in other {
        let e = acc.entry(s.clone()).or_insert(-1);
        if *q > *e {
            *e = *q;
        }
    }
}

/// Is `eff` covered by `pushed` (every component already at/under the push cursor)?
fn covered(eff: &VersionVector, pushed: &VersionVector) -> bool {
    eff.iter().all(|(s, q)| pushed.get(s).copied().unwrap_or(-1) >= *q)
}

/// Fold `main` rows restricted to `frontier` → live content files (`path -> (asp
/// content hash, merge class)`). A row is in scope iff it is visible on `main` and
/// `seq <= frontier[site]` (a site absent from the frontier is excluded — cap -1).
/// Directories and deletes carry no git tree entry, so they are dropped.
fn fold_at_frontier(
    engine: &Engine,
    all_rows: &[LogRow],
    vis: &Visibility<'_>,
    frontier: &VersionVector,
) -> AspResult<BTreeMap<String, String>> {
    let scoped: Vec<LogRow> = all_rows
        .iter()
        .filter(|r| vis.sees(r))
        .filter(|r| (r.seq as i64) <= frontier.get(&r.site_id).copied().unwrap_or(-1))
        .cloned()
        .collect();
    let files = crate::fold::compute_files(&engine.store, &scoped)?;
    let mut out = BTreeMap::new();
    for f in files {
        if f.deleted || f.merge_class == MergeClass::Dir {
            continue;
        }
        if let Some(h) = f.result_hash {
            out.insert(f.path, h);
        }
    }
    Ok(out)
}

/// Assemble the git commit object body (git-bridge §5.2). Matches git's canonical
/// layout: `tree`, optional `parent`, `author`/`committer` at `ts +0000`, blank
/// line, message (newline-terminated).
fn commit_body(tree_hex: &str, parent: Option<&str>, author: &str, ts: i64, message: &str) -> Vec<u8> {
    let mut s = String::new();
    s.push_str(&format!("tree {tree_hex}\n"));
    if let Some(p) = parent {
        s.push_str(&format!("parent {p}\n"));
    }
    s.push_str(&format!("author {author} {ts} +0000\n"));
    s.push_str(&format!("committer {author} {ts} +0000\n"));
    s.push('\n');
    s.push_str(message);
    if !message.ends_with('\n') {
        s.push('\n');
    }
    s.into_bytes()
}

/// Record a new git object (dedup within this synthesis + against the remote store)
/// and return its 20-byte oid.
fn emit(
    kind: GitObjectKind,
    content: Vec<u8>,
    store: &RemoteStore,
    objects: &mut Vec<(GitObjectKind, Vec<u8>)>,
    seen: &mut HashSet<String>,
) -> [u8; 20] {
    let oid = git_oid_bytes(kind, &content);
    let hexid = hex::encode(oid);
    if seen.insert(hexid.clone()) && !store.has(&hexid) {
        objects.push((kind, content));
    }
    oid
}

/// Build the git tree object rooted at `prefix` from the folded `files` (path -> asp
/// content hash) plus any carried-forward `gitlinks` (path -> submodule commit oid),
/// recursively. Emits each new blob + subtree; returns the tree oid. Per-entry mode
/// comes from `modes` (git-bridge §3.3/R4). Tree entry + sort encoding mirrors
/// `gitexport.rs` (mode + ' ' + name + NUL + 20-byte oid; dirs sort as `name/`).
#[allow(clippy::too_many_arguments)]
fn build_tree_object(
    prefix: &str,
    files: &BTreeMap<String, String>,
    gitlinks: &BTreeMap<String, [u8; 20]>,
    modes: &ModeTable,
    engine: &Engine,
    store: &RemoteStore,
    objects: &mut Vec<(GitObjectKind, Vec<u8>)>,
    seen: &mut HashSet<String>,
) -> AspResult<[u8; 20]> {
    struct Entry {
        mode: &'static str,
        name: String,
        oid: [u8; 20],
    }
    let mut entries: Vec<Entry> = Vec::new();
    let mut subdirs: BTreeSet<String> = BTreeSet::new();

    // Direct content-file leaves under this prefix.
    for (path, hash) in files {
        let Some(rel) = path.strip_prefix(prefix) else { continue };
        if rel.is_empty() {
            continue;
        }
        match rel.find('/') {
            Some(i) => {
                subdirs.insert(rel[..i].to_string());
            }
            None => {
                let bytes = engine.store.get_blob(hash)?.unwrap_or_default();
                let mode = modes.file_mode(path);
                let oid = emit(GitObjectKind::Blob, bytes, store, objects, seen);
                entries.push(Entry { mode, name: rel.to_string(), oid });
            }
        }
    }

    // Direct gitlink leaves (submodule pointers; no blob, oid = the recorded commit).
    for (path, oid) in gitlinks {
        let Some(rel) = path.strip_prefix(prefix) else { continue };
        if rel.is_empty() {
            continue;
        }
        match rel.find('/') {
            Some(i) => {
                subdirs.insert(rel[..i].to_string());
            }
            None => {
                entries.push(Entry { mode: "160000", name: rel.to_string(), oid: *oid });
            }
        }
    }

    // Subtrees.
    for name in &subdirs {
        let child_prefix = format!("{prefix}{name}/");
        let oid = build_tree_object(&child_prefix, files, gitlinks, modes, engine, store, objects, seen)?;
        entries.push(Entry { mode: "40000", name: name.clone(), oid });
    }

    entries.sort_by_key(|e| {
        let mut k = e.name.as_bytes().to_vec();
        if e.mode == "40000" {
            k.push(b'/');
        }
        k
    });

    let mut payload = Vec::new();
    for e in &entries {
        payload.extend_from_slice(e.mode.as_bytes());
        payload.push(b' ');
        payload.extend_from_slice(e.name.as_bytes());
        payload.push(0);
        payload.extend_from_slice(&e.oid);
    }
    Ok(emit(GitObjectKind::Tree, payload, store, objects, seen))
}

/// Collect `path -> commit oid` for every gitlink (mode `160000`) reachable from
/// `commit_sha`'s tree, so synthesis preserves submodules across pushes even though
/// they have no fold content (git-bridge §3.3).
fn collect_gitlinks(store: &RemoteStore, commit_sha: &str) -> BTreeMap<String, [u8; 20]> {
    let mut out = BTreeMap::new();
    let Some((kind, content)) = store.get_object(commit_sha) else { return out };
    if kind != GitObjectKind::Commit {
        return out;
    }
    // The `tree <hex>` header line.
    let mut tree_hex = None;
    for line in content.split(|&b| b == b'\n') {
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix(b"tree ") {
            tree_hex = std::str::from_utf8(rest).ok().map(|s| s.trim().to_string());
            break;
        }
    }
    if let Some(t) = tree_hex {
        walk_gitlinks(store, &t, "", &mut out);
    }
    out
}

fn walk_gitlinks(store: &RemoteStore, tree_sha: &str, prefix: &str, out: &mut BTreeMap<String, [u8; 20]>) {
    let Some((kind, content)) = store.get_object(tree_sha) else { return };
    if kind != GitObjectKind::Tree {
        return;
    }
    let mut i = 0;
    while i < content.len() {
        let Some(sp) = content[i..].iter().position(|&b| b == b' ').map(|p| i + p) else { break };
        let mode = std::str::from_utf8(&content[i..sp]).unwrap_or("");
        let Some(nul) = content[sp + 1..].iter().position(|&b| b == 0).map(|p| sp + 1 + p) else { break };
        let name = String::from_utf8_lossy(&content[sp + 1..nul]).to_string();
        let oid_start = nul + 1;
        if oid_start + 20 > content.len() {
            break;
        }
        let mut oid = [0u8; 20];
        oid.copy_from_slice(&content[oid_start..oid_start + 20]);
        i = oid_start + 20;
        let path = if prefix.is_empty() { name } else { format!("{prefix}{name}") };
        match mode {
            "160000" => {
                out.insert(path, oid);
            }
            "40000" | "040000" => {
                walk_gitlinks(store, &hex::encode(oid), &format!("{path}/"), out);
            }
            _ => {}
        }
    }
}

// ===========================================================================
// Push driver (git-bridge §5.2 + §9)
// ===========================================================================

/// The outcome of a [`push`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushReport {
    /// No unpushed plans (or already at the remote tip) — nothing sent.
    Nothing,
    /// New commits were pushed.
    Pushed {
        /// The new remote tip sha.
        pushed_sha: String,
        /// Number of commits sent.
        commits_pushed: usize,
        /// Number of plans that became commits.
        plans_pushed: usize,
    },
}

/// Push the vault's unpushed plans as real git commits (git-bridge §5.2). Loads the
/// remote + auth, synthesizes, `write_pack` + `push_pack`; handles the idempotent
/// race (ref already at our tip → success) and the bounded non-fast-forward retry
/// (a human pushed mid-cycle → pull → re-synthesize → retry).
pub async fn push(
    engine: &Engine,
    remote_id: &str,
    on_progress: impl Fn(&str),
) -> AspResult<PushReport> {
    let modes = ModeTable::load(engine)?;

    for _attempt in 0..MAX_PUSH_ATTEMPTS {
        let row = engine
            .store
            .git_remote_get(remote_id)?
            .ok_or_else(|| AspError::NotFound(format!("no git remote configured (id {remote_id})")))?;
        if row.frozen {
            return Err(AspError::Invalid(
                "remote is frozen (upstream history was rewritten) — run `asp git rebaseline` first".into(),
            ));
        }
        let spec = crate::gitremote::spec_from_row(&row)?;
        let store = RemoteStore::open(&engine.asp_dir, remote_id)?;
        let all_rows = engine.store.all_rows()?;
        let plans = gather_plans(engine, &all_rows)?;
        let synth = synthesize_commits(engine, &store, &row, &plans, &modes)?;

        if synth.plans_pushed == 0 || synth.tip_sha.is_empty() || synth.tip_sha == synth.base_sha {
            return Ok(PushReport::Nothing);
        }

        let push_ref = resolve_push_ref(&row);
        on_progress("pushing");
        let result = push_pack(
            &spec,
            &push_ref,
            &synth.base_sha,
            &synth.tip_sha,
            write_pack(&synth.objects_to_push),
        )
        .await;

        match result {
            Ok(_) => {
                return finish_push(engine, remote_id, &store, &synth);
            }
            Err(e) => {
                // A non-fast-forward rejection (a human pushed between our fetch and
                // push, or we raced another bridge). git hosts phrase this several
                // ways ("non-fast-forward", "fetch first", "incorrect old value").
                if !is_non_fast_forward(&e) {
                    return Err(e.into());
                }
                on_progress("checking remote");
                // Idempotent race (git-bridge §5.2): another node may have pushed the
                // identical tip. If the ref is already at our tip, treat as success.
                if ref_is_at(&spec, &push_ref, &synth.tip_sha).await? {
                    return finish_push(engine, remote_id, &store, &synth);
                }
                // Otherwise pull to ingest the upstream advance (raises the ingest
                // frontier), then re-synthesize onto the new tip and retry (git-bridge §9).
                on_progress("fetching");
                match crate::gitremote::pull_once(engine, remote_id, None).await? {
                    crate::gitremote::PullReport::Frozen => {
                        return Err(AspError::Invalid(
                            "upstream history was rewritten — run `asp git rebaseline`".into(),
                        ));
                    }
                    crate::gitremote::PullReport::UpToDate => {
                        // Nothing new upstream, yet the push was rejected — we cannot
                        // make progress by retrying.
                        return Err(e.into());
                    }
                    crate::gitremote::PullReport::Updated { .. } => { /* retry */ }
                }
            }
        }
    }
    Err(GitBridgeError::NonFastForward.into())
}

/// Whether a push error is a non-fast-forward / stale-base rejection (a human pushed
/// mid-cycle, git-bridge §9). `gitbridge` maps GitHub's phrasings to
/// [`GitBridgeError::NonFastForward`]; a bare `git-http-backend` instead rejects a
/// stale `old` value as `"incorrect old value provided"`, surfaced as `Rejected` — so
/// catch that too rather than treating it as a fatal push failure.
fn is_non_fast_forward(e: &GitBridgeError) -> bool {
    match e {
        GitBridgeError::NonFastForward => true,
        GitBridgeError::Rejected(m) => {
            let l = m.to_ascii_lowercase();
            l.contains("incorrect old value")
                || l.contains("fetch first")
                || l.contains("non-fast")
                || l.contains("not a fast forward")
                || l.contains("stale")
        }
        _ => false,
    }
}

/// Persist the push cursor + stage the pushed objects locally so a later synthesis
/// sees them as already-present (git-bridge §5.2).
fn finish_push(engine: &Engine, remote_id: &str, store: &RemoteStore, synth: &SynthOutput) -> AspResult<PushReport> {
    for (kind, content) in &synth.objects_to_push {
        store.write_loose_object(*kind, content)?;
    }
    let frontier_json = serde_json::to_string(&synth.pushed_frontier)
        .map_err(|e| AspError::Protocol(format!("serialize pushed frontier: {e}")))?;
    engine.store.git_remote_set_pushed(remote_id, &synth.tip_sha, &frontier_json)?;
    Ok(PushReport::Pushed {
        pushed_sha: synth.tip_sha.clone(),
        commits_pushed: synth.commits.len(),
        plans_pushed: synth.plans_pushed,
    })
}

/// The full ref name a push targets: `push_ref` if set, else the tracked remote ref,
/// else `refs/heads/<default branch>`. A short name is normalized under `refs/heads/`.
fn resolve_push_ref(row: &GitRemoteRow) -> String {
    let raw = row
        .push_ref
        .clone()
        .or_else(|| row.remote_ref.clone())
        .or_else(|| row.default_branch.clone().map(|b| format!("refs/heads/{b}")))
        .unwrap_or_else(|| "refs/heads/main".to_string());
    if raw.starts_with("refs/") {
        raw
    } else {
        format!("refs/heads/{raw}")
    }
}

/// Re-`ls-remote` and check whether `ref_name` already points at `tip` (the
/// idempotent-race success condition, git-bridge §5.2).
async fn ref_is_at(spec: &GitRemoteSpec, ref_name: &str, tip: &str) -> AspResult<bool> {
    let refs = crate::gitbridge::ls_remote(spec).await?;
    Ok(refs.refs.iter().any(|r| r.name == ref_name && r.oid == tip))
}

// ===========================================================================
// Pending diff (git-bridge §5.3 — pre-fill messages; later drives interval/llm)
// ===========================================================================

/// The pending change set since the last plan/ingest frontier (git-bridge §5.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDiff {
    /// Number of paths that were added, removed, or modified.
    pub files_changed: usize,
    /// The changed paths, sorted.
    pub paths: Vec<String>,
    /// A concatenated unified diff (per changed path).
    pub unified: String,
}

/// Compute the diff between the fold at the last plan/ingest frontier and the current
/// `main` fold (git-bridge §5.3). Used to pre-fill `asp git push` messages and, later,
/// to drive the `interval`/`llm` policies. Correct-but-simple: text via `similar`.
pub fn pending_git_diff(engine: &Engine, remote_id: &str) -> AspResult<PendingDiff> {
    let row = engine
        .store
        .git_remote_get(remote_id)?
        .ok_or_else(|| AspError::NotFound(format!("no git remote configured (id {remote_id})")))?;
    let all_rows = engine.store.all_rows()?;
    let bs = BranchSet::new(engine.store.branches()?);
    let vis = bs.visibility(MAIN_BRANCH_ID);

    // Base frontier = the last plan's effective frontier if any, else the ingest floor.
    let floor = ingest_floor(engine, &all_rows, row.last_ingested_sha.as_deref())?;
    let plans = gather_plans(engine, &all_rows)?;
    let mut base_frontier = floor.clone();
    for p in &plans {
        vv_max_into(&mut base_frontier, &p.frontier);
    }

    let main_rows: Vec<LogRow> = all_rows.iter().filter(|r| vis.sees(r)).cloned().collect();
    let current_frontier = version_vector_of(&main_rows);

    let base_files = fold_at_frontier(engine, &all_rows, &vis, &base_frontier)?;
    let cur_files = fold_at_frontier(engine, &all_rows, &vis, &current_frontier)?;

    let read = |h: &str| -> Vec<u8> { engine.store.get_blob(h).ok().flatten().unwrap_or_default() };

    let mut paths: Vec<String> = Vec::new();
    let mut unified = String::new();
    let all_paths: BTreeSet<&String> = base_files.keys().chain(cur_files.keys()).collect();
    for path in all_paths {
        let before = base_files.get(path);
        let after = cur_files.get(path);
        if before == after {
            continue;
        }
        paths.push(path.clone());
        let a = before.map(|h| read(h)).unwrap_or_default();
        let b = after.map(|h| read(h)).unwrap_or_default();
        let a_text = String::from_utf8_lossy(&a);
        let b_text = String::from_utf8_lossy(&b);
        let diff = similar::TextDiff::from_lines(a_text.as_ref(), b_text.as_ref());
        unified.push_str(
            &diff
                .unified_diff()
                .context_radius(3)
                .header(&format!("a/{path}"), &format!("b/{path}"))
                .to_string(),
        );
    }

    Ok(PendingDiff {
        files_changed: paths.len(),
        paths,
        unified,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_body_layout_is_canonical() {
        let body = commit_body("t".repeat(40).as_str(), Some(&"p".repeat(40)), "A <a@x>", 1700000000, "hello");
        let s = String::from_utf8(body).unwrap();
        assert!(s.starts_with(&format!("tree {}\nparent {}\nauthor A <a@x> 1700000000 +0000\ncommitter A <a@x> 1700000000 +0000\n\nhello\n", "t".repeat(40), "p".repeat(40))));
        // Root commit (no parent) omits the parent line.
        let root = String::from_utf8(commit_body(&"t".repeat(40), None, "A <a@x>", 1, "m\n")).unwrap();
        assert!(!root.contains("parent "));
        assert!(root.ends_with("\n\nm\n"));
    }

    #[test]
    fn covered_and_vv_max() {
        let mut acc: VersionVector = VersionVector::new();
        vv_max_into(&mut acc, &VersionVector::from([("a".to_string(), 3i64)]));
        vv_max_into(&mut acc, &VersionVector::from([("a".to_string(), 1i64), ("b".to_string(), 5)]));
        assert_eq!(acc.get("a"), Some(&3));
        assert_eq!(acc.get("b"), Some(&5));
        // Covered when every component is at/under the cursor.
        assert!(covered(&VersionVector::from([("a".to_string(), 3i64)]), &acc));
        assert!(!covered(&VersionVector::from([("a".to_string(), 4i64)]), &acc));
        // A site absent from the cursor is not covered.
        assert!(!covered(&VersionVector::from([("c".to_string(), 0i64)]), &acc));
    }
}
