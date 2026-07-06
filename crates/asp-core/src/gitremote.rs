//! gitremote — the native **orchestration** layer of the git bridge (git-bridge §4,
//! §6.3, §8). It composes the already-tested pure modules — [`crate::gitbridge`]
//! (transport + local object store), [`crate::gitimport`] (pack → replay model),
//! [`crate::gitgenesis`] (model → sealed rows), [`crate::gitrecord`] (ledger
//! payloads) — into a small driver over a live [`Engine`]:
//!
//! * [`clone_from_git`] — one-shot clone of a git URL into a **pristine** vault:
//!   ls-remote → fetch → decode → deterministic genesis → paged integrate → persist
//!   the [`GitRemoteRow`] cursor + the mode cache (git-bridge §3).
//! * [`pull_once`] — an ongoing pull into a live vault: ls-remote → force-push guard
//!   (§4.4) → fetch delta → rebuild the DAG → ingest the new commits chained onto the
//!   imported chain (§4.2).
//! * [`git_status`] — read the cursor + a best-effort ahead count from the fold.
//! * [`rebaseline`] — the explicit recovery after an upstream history rewrite (§4.4).
//!
//! The async transport calls live here; the engine mutation is synchronous. This is
//! native-only (it borrows the on-disk [`Engine`] + [`crate::sqlite::SqliteStore`]);
//! the browser drives the same pure modules over a `fetch()` transport in `asp-wasm`.

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;

use crate::engine::Engine;
use crate::error::{AspError, AspResult};
use crate::gitbridge::{
    fetch_pack, ls_remote, remote_id, GitAuth, GitObjectKind, GitRemoteSpec, RemoteStore,
};
use crate::gitgenesis::{
    git_file_id, git_site_id, synthesize_genesis, synthesize_ingest, DbBlobSource, ImportedFile,
    IngestContext,
};
use crate::gitimport::{
    no_base_lookup, plan_import, GitImportError, GitObjKind, GitObjectDb, ImportOptions,
    ImportWarning, MAIN_LANE,
};
use crate::gitrecord::{build_commit_marker_row, build_ingest_row, GitCommitMarker, GitIngestRecord, GitRowIdentity};
use crate::gitwire::{parse_git_url, GitUrl};
use crate::identity::Identity;
use crate::log::{classify, Kind, LogRow, MergeClass, MAIN_BRANCH_ID};
use crate::memengine::MemEngine;
use crate::session::SessionVault;
use crate::sqlite::{GitRemoteRow, SqliteStore};
use crate::store::{BlobStore, MemBlobStore};
use crate::wire::{WireBlob, WireRow};

/// Warn (but proceed) above this decoded-pack size (git-bridge §3.4 pre-flight).
const SIZE_WARN_BYTES: u64 = 500 * 1024 * 1024;

/// Rows per `integrate_many` page — a large clone/ingest streams page-by-page under
/// `set_batch` so it folds once, without holding every row's blobs in memory at once.
const INTEGRATE_PAGE: usize = 1000;

/// Progress phase callback: `(phase, done, total)` where `phase` is
/// `"fetching" | "replaying" | "saving"` (git-bridge §7.2 clone phases).
pub type ProgressFn<'a> = &'a (dyn Fn(&str, u64, u64) + Sync);

// ===========================================================================
// Public option / report types
// ===========================================================================

/// Options for [`clone_from_git`].
#[derive(Default)]
pub struct CloneOptions<'a> {
    /// Import only the last `n` first-parent commits (+ side ancestry), fronted by a
    /// synthetic snapshot (git-bridge §3.4). `None` = full DAG.
    pub depth: Option<u32>,
    /// Clone into a fresh random `vault_id` instead of the repo-derived one, for two
    /// intentionally-separate vaults (git-bridge §3.2 escape hatch).
    pub new_identity: bool,
    /// Phase progress sink.
    pub on_progress: Option<ProgressFn<'a>>,
}

/// The result of a successful [`clone_from_git`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitCloneReport {
    /// The vault identity adopted (repo-derived, or random under `--new-identity`).
    pub vault_id: String,
    /// Number of imported commits.
    pub commits: usize,
    /// Names of the imported side branches (empty for a linear repo).
    pub branches: Vec<String>,
    /// Human-readable degraded-content notices (submodules, LFS).
    pub warnings: Vec<String>,
    /// The imported tip sha.
    pub tip_sha: String,
}

/// The outcome of a [`pull_once`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullReport {
    /// The remote ref was already ingested — nothing to do.
    UpToDate,
    /// An upstream force-push/history rewrite was detected; the bridge froze and
    /// appended no rows (git-bridge §4.4). Run [`rebaseline`] to recover.
    Frozen,
    /// New upstream commits were ingested.
    Updated {
        /// Number of newly-ingested commits.
        new_commits: usize,
        /// Names of side branches added by this pull.
        branches_added: Vec<String>,
    },
}

/// A snapshot of a remote's bridge state for the status chip / `asp git status`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GitStatus {
    pub remote_url: String,
    /// The commit sha the vault is currently at (the ingest cursor).
    pub at_sha: Option<String>,
    /// True after a force-push freeze (git-bridge §4.4).
    pub frozen: bool,
    /// Local content rows on `main` not authored by the repo site — a best-effort
    /// "unpushed" count (git-bridge §5; exact frontier accounting lands with push).
    pub ahead: usize,
    /// Pending upstream commits — `0` until a pull is run (v1 keeps this simple).
    pub behind: usize,
    /// The rollup policy (`manual` in v1).
    pub policy: String,
}

// ===========================================================================
// Credentials (git-bridge §8)
// ===========================================================================

const KEYRING_SERVICE: &str = "asp-git";

/// Tests (and CI without a secret store) set `ASP_GIT_DISABLE_KEYRING=1` so keyring
/// calls are skipped entirely — never touching the OS store.
fn keyring_disabled() -> bool {
    std::env::var("ASP_GIT_DISABLE_KEYRING")
        .map(|v| !v.is_empty() && v != "0")
        .unwrap_or(false)
}

fn keyring_get(auth_ref: &str) -> Option<String> {
    if keyring_disabled() {
        return None;
    }
    keyring::Entry::new(KEYRING_SERVICE, auth_ref).ok()?.get_password().ok()
}

fn keyring_set(auth_ref: &str, token: &str) -> bool {
    if keyring_disabled() {
        return false;
    }
    matches!(keyring::Entry::new(KEYRING_SERVICE, auth_ref), Ok(e) if e.set_password(token).is_ok())
}

/// Resolve the credentials to use for a remote (git-bridge §8). Order: explicit
/// `--token`/`ASP_GIT_TOKEN` → keyring entry named by `auth_ref` → SSH agent for an
/// `ssh://`/scp URL → anonymous. Never panics; a missing/locked keyring simply falls
/// through to the next source.
pub fn resolve_git_auth(url: &GitUrl, cli_token: Option<&str>, auth_ref: Option<&str>) -> GitAuth {
    if let Some(t) = cli_token.filter(|s| !s.is_empty()) {
        return GitAuth::Token(t.to_string());
    }
    if let Ok(t) = std::env::var("ASP_GIT_TOKEN") {
        if !t.is_empty() {
            return GitAuth::Token(t);
        }
    }
    if let Some(r) = auth_ref {
        if let Some(t) = keyring_get(r) {
            return GitAuth::Token(t);
        }
    }
    if matches!(url, GitUrl::Ssh { .. }) {
        return GitAuth::SshAgent;
    }
    GitAuth::Anonymous
}

/// Persist `token` for `remote_id` in the OS keyring under `asp-git/<remote_id>` and
/// return the `auth_ref` (the entry name) to record in `git_remotes` — or `None` when
/// no keyring backend is available (the token is then not persisted; the caller keeps
/// it only for this session). A token is **never** written to `git_remotes.url`, the
/// log, or any synced table (git-bridge §8).
pub fn store_git_token(remote_id: &str, token: &str) -> Option<String> {
    if token.is_empty() {
        return None;
    }
    if keyring_set(remote_id, token) {
        Some(remote_id.to_string())
    } else {
        None
    }
}

// ===========================================================================
// clone_from_git
// ===========================================================================

/// Clone a git remote into a **pristine** [`Engine`] (git-bridge §3). All-or-nothing:
/// rows fold only after the whole pack decodes and genesis synthesizes, so a torn
/// clone leaves no vault (§9). Persists the [`GitRemoteRow`] cursor + the mode cache.
pub async fn clone_from_git(
    engine: &Engine,
    spec: &GitRemoteSpec,
    opts: &CloneOptions<'_>,
) -> AspResult<GitCloneReport> {
    let progress = |phase: &str, d: u64, t: u64| {
        if let Some(p) = opts.on_progress {
            p(phase, d, t);
        }
    };

    // Clone only into a fresh vault (git-bridge §3.2). Check before any network.
    if !engine.is_pristine() {
        return Err(AspError::Invalid(
            "cannot clone a git remote into a non-empty vault — clone into a fresh directory".into(),
        ));
    }

    // 1. ls-remote → default-branch tip.
    let refs = ls_remote(spec).await?;
    let default_branch = refs
        .default_branch
        .clone()
        .ok_or_else(|| AspError::NotFound("remote advertises no default branch (empty repo?)".into()))?;
    let tip = refs
        .default_branch_oid()
        .ok_or_else(|| AspError::NotFound("remote default branch has no commits (empty repo)".into()))?
        .to_string();
    let remote_ref = format!("refs/heads/{default_branch}");
    let url = git_url_string(&spec.url);
    let rid = remote_id(&url);

    // 2/3. Fetch the pack (§3.4 size guard is best-effort: warn on a large pack).
    progress("fetching", 0, 0);
    let outcome = fetch_pack(spec, std::slice::from_ref(&tip), &[], opts.depth, |_| {}).await?;
    if outcome.pack.len() as u64 > SIZE_WARN_BYTES {
        tracing::warn!(
            bytes = outcome.pack.len(),
            "git clone: large pack (> {} MB) — this may take a while",
            SIZE_WARN_BYTES / (1024 * 1024)
        );
    }
    let mut rstore = RemoteStore::open(&engine.asp_dir, &rid)?;
    rstore.record_fetch(&outcome.pack, &[(remote_ref.clone(), tip.clone())])?;

    // 4. Decode + plan.
    progress("replaying", 0, 0);
    let db = GitObjectDb::from_pack(&outcome.pack, no_base_lookup).map_err(imp)?;
    let iopts = ImportOptions { depth: opts.depth, keep_imported_branches: false };
    let plan = plan_import(&db, &tip, &iopts).map_err(imp)?;

    // 5. Deterministic genesis → paged integrate under batch.
    let scratch = MemBlobStore::new();
    let g = synthesize_genesis(&plan, &DbBlobSource::new(&db), &scratch)?;
    let vault_id = if opts.new_identity { random_vault_id() } else { g.vault_id.clone() };
    engine.adopt_vault_id(&vault_id)?;

    progress("saving", 0, g.rows.len() as u64);
    engine.set_batch(true);
    let res = integrate_paged(engine, &g.rows, &scratch, &|d, t| progress("saving", d, t));
    engine.set_batch(false);
    res?;
    engine.materialize()?;

    // Persist the cursor + mode cache. A token (if any) goes to the keyring, only its
    // entry name to git_remotes (git-bridge §8).
    let auth_ref = match &spec.auth {
        GitAuth::Token(t) => store_git_token(&rid, t),
        _ => None,
    };
    let row = GitRemoteRow {
        remote_id: rid,
        url,
        push_ref: None,
        policy: "manual".into(),
        auth_ref,
        default_branch: Some(default_branch),
        last_ingested_sha: Some(tip.clone()),
        remote_ref: Some(remote_ref),
        root_sha: Some(plan.root_sha.clone()),
        frozen: false,
        last_pushed_sha: None,
        last_pushed_frontier: None,
    };
    engine.store.git_remote_upsert(&row)?;
    engine.store.git_mode_clear()?;
    persist_modes(&engine.store, &g.mode_table, &g.symlinks, &g.gitlinks)?;

    let branches: Vec<String> = plan
        .lanes
        .iter()
        .filter(|l| l.id != MAIN_LANE)
        .map(|l| l.name.clone())
        .collect();
    Ok(GitCloneReport {
        vault_id,
        commits: plan.commits.len(),
        branches,
        warnings: warning_strings(&plan.warnings),
        tip_sha: tip,
    })
}

// ===========================================================================
// pull_once
// ===========================================================================

/// Fetch and ingest new upstream commits into a live vault (git-bridge §4.2/§4.4).
/// Detects an upstream force-push and freezes rather than corrupting the timeline.
pub async fn pull_once(
    engine: &Engine,
    remote_id_str: &str,
    on_progress: Option<ProgressFn<'_>>,
) -> AspResult<PullReport> {
    let progress = |phase: &str, d: u64, t: u64| {
        if let Some(p) = on_progress {
            p(phase, d, t);
        }
    };

    let row = engine
        .store
        .git_remote_get(remote_id_str)?
        .ok_or_else(|| AspError::NotFound(format!("no git remote configured (id {remote_id_str})")))?;
    if row.frozen {
        // Persistent error state — refuse until an explicit rebaseline (git-bridge §4.4).
        return Ok(PullReport::Frozen);
    }
    let spec = spec_from_row(&row)?;
    let root_sha = row
        .root_sha
        .clone()
        .ok_or_else(|| AspError::Invalid("remote has no recorded root — clone it first".into()))?;

    // 1. ls-remote → the new tip on the tracked ref.
    let refs = ls_remote(&spec).await?;
    let default_branch = row
        .default_branch
        .clone()
        .or_else(|| refs.default_branch.clone())
        .ok_or_else(|| AspError::NotFound("remote advertises no default branch".into()))?;
    let remote_ref = row.remote_ref.clone().unwrap_or_else(|| format!("refs/heads/{default_branch}"));
    let new_tip = tip_for_ref(&refs, &remote_ref)?;

    let last = row.last_ingested_sha.clone();
    if last.as_deref() == Some(new_tip.as_str()) {
        return Ok(PullReport::UpToDate);
    }

    // 2. Fetch the delta (haves = last), exploding it into the object store.
    progress("fetching", 0, 0);
    let haves: Vec<String> = last.iter().cloned().collect();
    let outcome = fetch_pack(&spec, std::slice::from_ref(&new_tip), &haves, None, |_| {}).await?;
    let mut rstore = RemoteStore::open(&engine.asp_dir, remote_id_str)?;
    rstore.record_fetch(&outcome.pack, &[(remote_ref.clone(), new_tip.clone())])?;

    // 3. Force-push detection (§4.4): the new tip must descend from the last-ingested.
    if let Some(last_sha) = &last {
        if !rstore.is_ancestor(last_sha, &new_tip)? {
            engine.store.git_remote_set_frozen(remote_id_str, true)?;
            return Ok(PullReport::Frozen);
        }
    }

    // 4. Rebuild the full DAG from ALL accumulated objects (haves kept the *network*
    //    fetch small; the delta's parents attach to history we already exploded), plan
    //    the whole thing, and let synthesize_ingest skip already-seen commits.
    progress("replaying", 0, 0);
    let db = load_db_from_store(&rstore)?;
    let plan = plan_import(&db, &new_tip, &ImportOptions::default()).map_err(imp)?;

    let site = git_site_id(&root_sha);
    let ImportedMain { main_state, main_last_row, seen } = reconstruct_main_state(engine, &site)?;

    // New commits + the side branches they introduce (for the report).
    let new_shas: HashSet<&str> = plan
        .commits
        .iter()
        .filter(|c| !seen.contains(&c.sha))
        .map(|c| c.sha.as_str())
        .collect();
    let branches_added: Vec<String> = plan
        .lanes
        .iter()
        .filter(|l| l.id != MAIN_LANE && new_shas.contains(l.created_at_commit.as_str()))
        .map(|l| l.name.clone())
        .collect();

    let ctx = IngestContext {
        site_id: site,
        next_seq: engine.store.next_seq(&git_site_id(&root_sha))?,
        next_lamport: engine.store.next_lamport(0)?,
        remote_ref: remote_ref.clone(),
        main_state,
        main_last_row,
        seen,
    };
    let scratch = MemBlobStore::new();
    let out = synthesize_ingest(&plan, &ctx, &DbBlobSource::new(&db), &scratch)?;

    if out.rows.is_empty() {
        // Everything the fetch revealed was already ingested (another bridge won).
        engine.store.git_remote_set_ingested(remote_id_str, &new_tip, &remote_ref)?;
        return Ok(PullReport::UpToDate);
    }
    progress("saving", 0, out.rows.len() as u64);
    engine.set_batch(true);
    let res = integrate_paged(engine, &out.rows, &scratch, &|d, t| progress("saving", d, t));
    engine.set_batch(false);
    res?;
    engine.materialize()?;
    engine.store.git_remote_set_ingested(remote_id_str, &new_tip, &remote_ref)?;
    apply_mode_delta(&engine.store, &out.mode_table, &out.symlinks, &out.gitlinks)?;

    Ok(PullReport::Updated { new_commits: out.ledger.len(), branches_added })
}

// ===========================================================================
// git_status
// ===========================================================================

/// Read a remote's bridge status from the config + fold (git-bridge §4.1).
pub fn git_status(engine: &Engine, remote_id_str: &str) -> AspResult<GitStatus> {
    let row = engine
        .store
        .git_remote_get(remote_id_str)?
        .ok_or_else(|| AspError::NotFound(format!("no git remote configured (id {remote_id_str})")))?;
    let ahead = match &row.root_sha {
        Some(root) => count_local_main_rows(&engine.store, &git_site_id(root))?,
        None => 0,
    };
    Ok(GitStatus {
        remote_url: row.url,
        at_sha: row.last_ingested_sha,
        frozen: row.frozen,
        ahead,
        behind: 0,
        policy: row.policy,
    })
}

// ===========================================================================
// rebaseline (git-bridge §4.4)
// ===========================================================================

/// Recover from an upstream force-push (git-bridge §4.4): fetch the rewritten tip in
/// full, then author ONE synthetic snapshot batch = tree-diff(current imported main
/// state, new tip) chained onto the imported chain, tagged `rebaselined` in the
/// ledger. Clears the freeze and advances the cursor.
///
/// **Simplification (M3):** the diff base is the *imported* main state (the repo
/// site's chain tips), not the full local fold — so a raced local edit becomes an
/// ordinary concurrent fork the 3-way merge resolves, exactly like a normal ingest.
/// The batch is one synthetic commit; it does not reconstruct the rewritten history.
pub async fn rebaseline(engine: &Engine, remote_id_str: &str) -> AspResult<PullReport> {
    let row = engine
        .store
        .git_remote_get(remote_id_str)?
        .ok_or_else(|| AspError::NotFound(format!("no git remote configured (id {remote_id_str})")))?;
    let spec = spec_from_row(&row)?;
    let root_sha = row
        .root_sha
        .clone()
        .ok_or_else(|| AspError::Invalid("remote has no recorded root — clone it first".into()))?;

    // Fetch the rewritten tip in full — the rewrite may share little with what we
    // have, so a self-contained pack is the robust choice.
    let refs = ls_remote(&spec).await?;
    let default_branch = row
        .default_branch
        .clone()
        .or_else(|| refs.default_branch.clone())
        .ok_or_else(|| AspError::NotFound("remote advertises no default branch".into()))?;
    let remote_ref = row.remote_ref.clone().unwrap_or_else(|| format!("refs/heads/{default_branch}"));
    let new_tip = tip_for_ref(&refs, &remote_ref)?;

    let outcome = fetch_pack(&spec, std::slice::from_ref(&new_tip), &[], None, |_| {}).await?;
    let mut rstore = RemoteStore::open(&engine.asp_dir, remote_id_str)?;
    rstore.record_fetch(&outcome.pack, &[(remote_ref.clone(), new_tip.clone())])?;
    let db = GitObjectDb::from_pack(&outcome.pack, no_base_lookup).map_err(imp)?;

    // Recover the rewritten tip's FULL tree content by folding a throwaway genesis
    // into a MemEngine (reuses all the import machinery — no manual tree walk).
    let plan = plan_import(&db, &new_tip, &ImportOptions::default()).map_err(imp)?;
    let snap_store = MemBlobStore::new();
    let g = synthesize_genesis(&plan, &DbBlobSource::new(&db), &snap_store)?;
    let mem = MemEngine::create(Identity::from_seed(&[0u8; 32]), &g.vault_id);
    mem.integrate_many(&to_wires(&g.rows, &snap_store))?;
    let new_files = mem.files_map()?; // path -> bytes at the rewritten tip

    // Author the diff vs the current imported main state as one synthetic batch.
    let site = git_site_id(&root_sha);
    let ImportedMain { main_state, main_last_row, .. } = reconstruct_main_state(engine, &site)?;
    let current: BTreeMap<String, ImportedFile> =
        main_state.into_iter().map(|f| (f.path.clone(), f)).collect();

    let tip_commit = plan.commits.iter().find(|c| c.sha == new_tip);
    let ts = tip_commit.map(|c| c.committer_ts_ms / 1000).unwrap_or_else(now_unix_secs);

    let scratch = MemBlobStore::new();
    let mut rows: Vec<LogRow> = Vec::new();
    let mut seq = engine.store.next_seq(&site)?;
    let mut lamport = engine.store.next_lamport(0)?;
    let mut last_row = main_last_row.clone();
    let push = |rows: &mut Vec<LogRow>, last: &mut Option<String>, seq: &mut u64, lamport: &mut u64, row: LogRow| {
        *last = Some(row.id.clone());
        *seq += 1;
        *lamport += 1;
        rows.push(row);
    };

    // Creates + edits (deterministic path order).
    for (path, bytes) in &new_files {
        let new_hash = scratch.put_blob(bytes)?;
        let row = match current.get(path) {
            Some(f) if f.content_hash.as_deref() == Some(new_hash.as_str()) => continue, // unchanged
            Some(f) => snapshot_row(
                &site, seq, lamport, ts, &f.file_id, Kind::Edit, classify(path, bytes),
                Some(f.row_id.clone()), f.content_hash.clone(), Some(new_hash), None,
            ),
            None => {
                let fid = git_file_id(&root_sha, &new_tip, path);
                snapshot_row(
                    &site, seq, lamport, ts, &fid, Kind::Create, classify(path, bytes),
                    None, None, Some(new_hash), Some(path.clone()),
                )
            }
        };
        push(&mut rows, &mut last_row, &mut seq, &mut lamport, row);
    }
    // Deletes: paths gone from the rewritten tip.
    for (path, f) in &current {
        if !new_files.contains_key(path) {
            let row = snapshot_row(
                &site, seq, lamport, ts, &f.file_id, Kind::Delete, MergeClass::Text,
                Some(f.row_id.clone()), f.content_hash.clone(), None, None,
            );
            push(&mut rows, &mut last_row, &mut seq, &mut lamport, row);
        }
    }

    // Commit marker + a `rebaselined` ledger record.
    let marker = GitCommitMarker {
        sha: new_tip.clone(),
        author_name: tip_commit.map(|c| c.author_name.clone()).unwrap_or_default(),
        author_email: tip_commit.map(|c| c.author_email.clone()).unwrap_or_default(),
        committer_ts: ts,
        message: tip_commit.map(|c| c.message.clone()).unwrap_or_default(),
        parents: vec![],
        branch_id: MAIN_BRANCH_ID.to_string(),
    };
    let marker_ident = GitRowIdentity { site_id: site.clone(), lamport, seq, ts, parent: last_row.clone() };
    let marker_row = build_commit_marker_row(&scratch, &marker_ident, &marker)?;
    let marker_seq = seq;
    seq += 1;
    lamport += 1;
    rows.push(marker_row);

    let ingest = GitIngestRecord {
        commit_sha: new_tip.clone(),
        upto_site: site.clone(),
        upto_seq: marker_seq,
        modes: g.mode_table.clone(),
        symlinks: g.symlinks.clone(),
        gitlinks: g.gitlinks.clone(),
        remote_ref: remote_ref.clone(),
        rebaselined: true,
    };
    let ingest_ident = GitRowIdentity { site_id: site.clone(), lamport, seq, ts, parent: None };
    rows.push(build_ingest_row(&scratch, &ingest_ident, &ingest)?);

    engine.set_batch(true);
    let res = integrate_paged(engine, &rows, &scratch, &|_, _| {});
    engine.set_batch(false);
    res?;
    engine.materialize()?;

    engine.store.git_remote_set_frozen(remote_id_str, false)?;
    engine.store.git_remote_set_ingested(remote_id_str, &new_tip, &remote_ref)?;
    engine.store.git_mode_clear()?;
    persist_modes(&engine.store, &g.mode_table, &g.symlinks, &g.gitlinks)?;

    Ok(PullReport::Updated { new_commits: 1, branches_added: vec![] })
}

// ===========================================================================
// Shared helpers
// ===========================================================================

fn imp(e: GitImportError) -> AspError {
    AspError::Protocol(format!("git import: {e}"))
}

/// Build one sealed content row on `main` for the repo site (the rebaseline snapshot
/// batch, git-bridge §4.4).
#[allow(clippy::too_many_arguments)]
fn snapshot_row(
    site: &str,
    seq: u64,
    lamport: u64,
    ts: i64,
    file_id: &str,
    kind: Kind,
    mc: MergeClass,
    parent: Option<String>,
    base_hash: Option<String>,
    result_hash: Option<String>,
    path: Option<String>,
) -> LogRow {
    LogRow {
        site_id: site.to_string(),
        lamport,
        seq,
        ts,
        file_id: file_id.to_string(),
        kind,
        merge_class: mc,
        parent,
        base_hash,
        result_hash,
        path,
        branch_id: MAIN_BRANCH_ID.to_string(),
        merge_parent: None,
        sig: vec![],
        id: String::new(),
    }
    .seal()
}

fn now_unix_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn random_vault_id() -> String {
    hex::encode(rand::random::<[u8; 32]>())
}

/// Render a [`GitUrl`] back to a string for `remote_id` derivation + storage.
fn git_url_string(url: &GitUrl) -> String {
    match url {
        GitUrl::Https { base } => base.clone(),
        GitUrl::Ssh { user, host, port, path } => {
            let u = user.as_ref().map(|u| format!("{u}@")).unwrap_or_default();
            match port {
                Some(p) => format!("ssh://{u}{host}:{p}/{path}"),
                None => format!("{u}{host}:{path}"),
            }
        }
    }
}

/// Parse a stored remote URL back into a [`GitUrl`]. Accepts plain `http://` for
/// loopback/test servers (the strict [`parse_git_url`] only admits `https`/`ssh`).
fn parse_stored_url(url: &str) -> Option<GitUrl> {
    if let Some(u) = parse_git_url(url) {
        return Some(u);
    }
    if url.starts_with("http://") {
        return Some(GitUrl::Https { base: url.trim_end_matches('/').to_string() });
    }
    None
}

/// Build a [`GitRemoteSpec`] (parsed URL + resolved credentials) from a stored
/// remote row — shared by pull and the push driver (git-bridge §5.2).
pub fn spec_from_row(row: &GitRemoteRow) -> AspResult<GitRemoteSpec> {
    let url = parse_stored_url(&row.url)
        .ok_or_else(|| AspError::Invalid(format!("unparseable stored git remote url: {}", row.url)))?;
    let auth = resolve_git_auth(&url, None, row.auth_ref.as_deref());
    Ok(GitRemoteSpec::new(url, auth))
}

/// The oid the tracked ref points at, falling back to the advertised default branch.
fn tip_for_ref(refs: &crate::gitbridge::RemoteRefs, remote_ref: &str) -> AspResult<String> {
    refs.refs
        .iter()
        .find(|r| r.name == remote_ref)
        .map(|r| r.oid.clone())
        .or_else(|| refs.default_branch_oid().map(|s| s.to_string()))
        .ok_or_else(|| AspError::NotFound(format!("remote ref {remote_ref} not found")))
}

fn import_kind(k: GitObjectKind) -> GitObjKind {
    match k {
        GitObjectKind::Commit => GitObjKind::Commit,
        GitObjectKind::Tree => GitObjKind::Tree,
        GitObjectKind::Blob => GitObjKind::Blob,
        GitObjectKind::Tag => GitObjKind::Tag,
    }
}

/// Rebuild an in-memory object db from every loose object the store holds (all
/// previously-fetched objects + this fetch's delta). O(object count) reads; fine for
/// M3 (a future optimization can inject only the boundary bases the delta attaches to).
fn load_db_from_store(rstore: &RemoteStore) -> AspResult<GitObjectDb> {
    let mut db = GitObjectDb::new();
    let objects = rstore.root().join("objects");
    let shards = match std::fs::read_dir(&objects) {
        Ok(s) => s,
        Err(_) => return Ok(db),
    };
    for shard in shards.flatten() {
        let prefix = shard.file_name().to_string_lossy().to_string();
        if prefix.len() != 2 {
            continue;
        }
        let Ok(files) = std::fs::read_dir(shard.path()) else { continue };
        for f in files.flatten() {
            let rest = f.file_name().to_string_lossy().to_string();
            let sha = format!("{prefix}{rest}");
            if sha.len() != 40 {
                continue;
            }
            if let Some((k, body)) = rstore.get_object(&sha) {
                db.insert_loose(import_kind(k), &body).map_err(imp)?;
            }
        }
    }
    Ok(db)
}

/// Bundle each row with its `base_hash`/`result_hash` blobs (from `store`) into a
/// [`WireRow`], the shape `integrate_many` consumes (mirrors the memengine clone
/// receiver + the gitgenesis fold test).
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

/// Integrate `rows` page-by-page under batch, folding once (the caller must have
/// enabled `set_batch` and must `materialize()` afterwards).
fn integrate_paged(
    engine: &Engine,
    rows: &[LogRow],
    store: &dyn BlobStore,
    progress: &dyn Fn(u64, u64),
) -> AspResult<()> {
    let total = rows.len() as u64;
    let mut done = 0u64;
    for chunk in rows.chunks(INTEGRATE_PAGE) {
        engine.integrate_many(&to_wires(chunk, store))?;
        done += chunk.len() as u64;
        progress(done, total);
    }
    Ok(())
}

fn persist_modes(
    store: &SqliteStore,
    modes: &[(String, u32)],
    symlinks: &[String],
    gitlinks: &[String],
) -> AspResult<()> {
    apply_mode_delta(store, modes, symlinks, gitlinks)
}

fn apply_mode_delta(
    store: &SqliteStore,
    modes: &[(String, u32)],
    symlinks: &[String],
    gitlinks: &[String],
) -> AspResult<()> {
    for (p, m) in modes {
        store.git_mode_put(p, *m, "file")?;
    }
    for p in symlinks {
        store.git_mode_put(p, 0o120000, "symlink")?;
    }
    for p in gitlinks {
        store.git_mode_put(p, 0o160000, "gitlink")?;
    }
    Ok(())
}

fn warning_strings(warnings: &[ImportWarning]) -> Vec<String> {
    warnings
        .iter()
        .map(|w| match w {
            ImportWarning::Submodule { path, .. } => {
                format!("submodule at {path} imported as nothing (gitlink not materialized)")
            }
            ImportWarning::LfsPointers { paths } => {
                format!("{} git-LFS pointer file(s) imported as pointer text (not smudged)", paths.len())
            }
        })
        .collect()
}

/// The imported-chain state on `main`, reconstructed from the repo site's rows (the
/// tips an ongoing ingest chains onto — NOT the local-edit fold, git-bridge §4.2).
struct ImportedMain {
    main_state: Vec<ImportedFile>,
    main_last_row: Option<String>,
    /// Commit shas that already have a `GitIngest` row (skip on ingest).
    seen: HashSet<String>,
}

fn reconstruct_main_state(engine: &Engine, site: &str) -> AspResult<ImportedMain> {
    let rows = engine.store.rows_after(site, -1)?; // all repo-site rows, seq order
    let mut path_fid: BTreeMap<String, String> = BTreeMap::new();
    let mut fid_path: BTreeMap<String, String> = BTreeMap::new();
    let mut file_tip: BTreeMap<String, (String, Option<String>)> = BTreeMap::new();
    let mut main_last_row: Option<String> = None;
    let mut seen: HashSet<String> = HashSet::new();

    for r in &rows {
        if r.kind == Kind::GitIngest {
            if let Some(sha) = &r.path {
                seen.insert(sha.clone());
            }
        }
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
            Some(ImportedFile { path, file_id: fid, row_id, content_hash })
        })
        .collect();
    Ok(ImportedMain { main_state, main_last_row, seen })
}

/// Best-effort "ahead" count: content rows on `main` not authored by the repo site
/// (git-bridge §5 unpushed frontier — exact accounting arrives with the push slice).
fn count_local_main_rows(store: &SqliteStore, site: &str) -> AspResult<usize> {
    let n: i64 = store.conn().query_row(
        "SELECT COUNT(*) FROM log WHERE branch_id='main' AND site_id != ?1 \
         AND kind IN ('create','edit','delete','rename')",
        [site],
        |r| r.get(0),
    )?;
    Ok(n as usize)
}

// ===========================================================================
// Unit tests (native, no network, no real keyring)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_remote_and_mode_store_round_trip() {
        let store = SqliteStore::open_memory().unwrap();
        let row = GitRemoteRow {
            remote_id: "abcd1234abcd1234".into(),
            url: "https://example.com/owner/repo".into(),
            push_ref: None,
            policy: "manual".into(),
            auth_ref: Some("abcd1234abcd1234".into()),
            default_branch: Some("main".into()),
            last_ingested_sha: Some("f".repeat(40)),
            remote_ref: Some("refs/heads/main".into()),
            root_sha: Some("a".repeat(40)),
            frozen: false,
            last_pushed_sha: None,
            last_pushed_frontier: None,
        };
        store.git_remote_upsert(&row).unwrap();
        assert_eq!(store.git_remote_get(&row.remote_id).unwrap().as_ref(), Some(&row));
        assert_eq!(store.git_remote_list().unwrap(), vec![row.clone()]);

        // frozen + cursor updates.
        store.git_remote_set_frozen(&row.remote_id, true).unwrap();
        assert!(store.git_remote_get(&row.remote_id).unwrap().unwrap().frozen);
        store.git_remote_set_ingested(&row.remote_id, &"b".repeat(40), "refs/heads/main").unwrap();
        let got = store.git_remote_get(&row.remote_id).unwrap().unwrap();
        assert_eq!(got.last_ingested_sha.as_deref(), Some("b".repeat(40).as_str()));

        // mode cache.
        store.git_mode_put("bin/run", 0o100755, "file").unwrap();
        store.git_mode_put("link", 0o120000, "symlink").unwrap();
        let all = store.git_mode_get_all().unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|(p, m, k)| p == "bin/run" && *m == 0o100755 && k == "file"));
        store.git_mode_clear().unwrap();
        assert!(store.git_mode_get_all().unwrap().is_empty());

        store.git_remote_remove(&row.remote_id).unwrap();
        assert!(store.git_remote_get(&row.remote_id).unwrap().is_none());
    }

    #[test]
    fn resolve_git_auth_precedence() {
        // Keep the OS keyring untouched for the whole test.
        std::env::set_var("ASP_GIT_DISABLE_KEYRING", "1");
        std::env::remove_var("ASP_GIT_TOKEN");

        let https = GitUrl::Https { base: "https://example.com/o/r".into() };
        let ssh = GitUrl::Ssh { user: Some("git".into()), host: "example.com".into(), port: None, path: "o/r".into() };

        // 1. explicit token wins over everything (even for ssh).
        assert!(matches!(resolve_git_auth(&ssh, Some("cli-tok"), Some("some-ref")), GitAuth::Token(t) if t == "cli-tok"));

        // 2. env token, when no explicit token.
        std::env::set_var("ASP_GIT_TOKEN", "env-tok");
        assert!(matches!(resolve_git_auth(&https, None, None), GitAuth::Token(t) if t == "env-tok"));
        std::env::remove_var("ASP_GIT_TOKEN");

        // 3. keyring disabled → miss falls through: ssh → agent, https → anonymous.
        assert!(matches!(resolve_git_auth(&ssh, None, Some("missing-ref")), GitAuth::SshAgent));
        assert!(matches!(resolve_git_auth(&https, None, Some("missing-ref")), GitAuth::Anonymous));
        assert!(matches!(resolve_git_auth(&https, None, None), GitAuth::Anonymous));

        std::env::remove_var("ASP_GIT_DISABLE_KEYRING");
    }

    #[test]
    fn store_git_token_no_keyring_returns_none() {
        std::env::set_var("ASP_GIT_DISABLE_KEYRING", "1");
        assert_eq!(store_git_token("rid", "tok"), None);
        assert_eq!(store_git_token("rid", ""), None);
        std::env::remove_var("ASP_GIT_DISABLE_KEYRING");
    }

    #[test]
    fn url_round_trips_and_remote_id_is_stable() {
        let u = GitUrl::Https { base: "https://github.com/o/r".into() };
        assert_eq!(git_url_string(&u), "https://github.com/o/r");
        // http:// stored urls parse back (test-server shape).
        assert!(matches!(parse_stored_url("http://127.0.0.1:9/x.git"), Some(GitUrl::Https { .. })));
        assert!(parse_stored_url("not a url").is_none());
    }
}
