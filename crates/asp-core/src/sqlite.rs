//! The SQLite substrate (§Data model & storage). One mature embedded dependency
//! gives incremental durable persistence, transactional integrity, a single-file
//! store, and SQL query for agents. The schema is exactly the spec's: an
//! append-only `log` (source of truth) + content-addressed `blobs`, a
//! materialized `files`, the node-local `authorized_keys`/`peers` tables, synced
//! `config`, `snapshots`, and the v1-inert `embeddings` table.
//!
//! The Lamport and per-device `seq` counters are **derived from the durable
//! log** (`max(lamport)+1`, `max(seq where site)+1`), so they survive restart by
//! construction and need no side counter.

use crate::authkeys::AuthKey;
use crate::branch::Branch;
use crate::error::AspResult;
use crate::log::{Kind, LogRow, MergeClass};
use crate::store::{BlobStore, FileRow};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::BTreeMap;
use std::path::Path;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS blobs(content_hash TEXT PRIMARY KEY, bytes BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS log(
  id TEXT PRIMARY KEY, site_id TEXT NOT NULL, lamport INTEGER NOT NULL, seq INTEGER NOT NULL,
  ts INTEGER NOT NULL, file_id TEXT NOT NULL, kind TEXT NOT NULL, merge_class TEXT NOT NULL,
  parent TEXT, base_hash TEXT, result_hash TEXT, path TEXT,
  branch_id TEXT NOT NULL DEFAULT 'main', merge_parent TEXT, sig BLOB,
  UNIQUE(site_id, seq)
);
CREATE INDEX IF NOT EXISTS log_file ON log(file_id);
CREATE INDEX IF NOT EXISTS log_site ON log(site_id, seq);
-- NOTE: the `log_branch` index on log(branch_id) is created in `migrate_branching`,
-- NOT here. On a pre-branching DB the `log` table already exists (so the CREATE
-- TABLE above is a no-op) and still lacks `branch_id`; indexing it before the
-- migration's ALTER adds the column fails the whole batch with "no such column".
-- Branch records (§2.1). `fork_vv` is JSON (site_id -> max seq at the fork).
-- Synced as Kind::Branch rows in P4; for now node-local + replicated by checkout/
-- merge broadcasts. `main` is implicit (BranchSet injects it) so the table may be
-- empty on a single-branch vault — byte-identical to today.
CREATE TABLE IF NOT EXISTS branches(
  branch_id TEXT PRIMARY KEY, name TEXT NOT NULL, parent TEXT,
  fork_vv TEXT NOT NULL DEFAULT '{}', created_lamport INTEGER NOT NULL DEFAULT 0,
  created_ts INTEGER NOT NULL DEFAULT 0, deleted INTEGER NOT NULL DEFAULT 0
);
-- The checked-out branch (HEAD). Per-device, never synced (§7).
CREATE TABLE IF NOT EXISTS head(singleton INTEGER PRIMARY KEY CHECK(singleton=0), branch_id TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS files(
  file_id TEXT PRIMARY KEY, path TEXT, result_hash TEXT, merge_class TEXT,
  deleted INTEGER NOT NULL DEFAULT 0, lamport INTEGER, site_id TEXT, conflict INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS files_path ON files(path);
CREATE TABLE IF NOT EXISTS fold_cache(step_key TEXT PRIMARY KEY, output_hash TEXT);
CREATE TABLE IF NOT EXISTS snapshots(
  snapshot_id TEXT PRIMARY KEY, created_lamport INTEGER, label TEXT, tree_hash TEXT,
  created_ts INTEGER, manifest TEXT
);
CREATE TABLE IF NOT EXISTS embeddings(content_hash TEXT, model_id TEXT, vector BLOB, PRIMARY KEY(content_hash, model_id));
CREATE TABLE IF NOT EXISTS peer_state(site_id TEXT PRIMARY KEY, last_seq INTEGER);
CREATE TABLE IF NOT EXISTS peers(url TEXT PRIMARY KEY, node_id TEXT, pinned_at INTEGER);
CREATE TABLE IF NOT EXISTS authorized_keys(
  ssh_pubkey TEXT PRIMARY KEY, node_id TEXT NOT NULL, expires_at INTEGER, never INTEGER NOT NULL DEFAULT 0,
  added_at INTEGER, source TEXT
);
CREATE TABLE IF NOT EXISTS config(key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE IF NOT EXISTS git_blobs(content_hash TEXT PRIMARY KEY, git_oid TEXT NOT NULL);
-- Per-file (mtime, size) -> content hash cache, so the startup reconcile can skip
-- reading + hashing files whose stat is unchanged (the dominant cost on a big
-- working tree). Purely a local performance cache: never synced, and a miss only
-- costs a re-read, so it can be dropped at any time without affecting correctness.
CREATE TABLE IF NOT EXISTS fs_stat(path TEXT PRIMARY KEY, mtime_ns INTEGER NOT NULL, size INTEGER NOT NULL, hash TEXT NOT NULL);
-- git-bridge remote config (git-bridge §4.1/§6.3). Node-private: URLs, auth refs,
-- and ingest cursor differ per node, so this table is NEVER synced. `root_sha` is
-- the imported main-chain root (the key for the derived repo `site_id`, needed by
-- ongoing pulls). `auth_ref` names a keyring entry, never a token itself (§8).
CREATE TABLE IF NOT EXISTS git_remotes(
  remote_id TEXT PRIMARY KEY, url TEXT NOT NULL, push_ref TEXT,
  policy TEXT NOT NULL DEFAULT 'manual', auth_ref TEXT, default_branch TEXT,
  last_ingested_sha TEXT, remote_ref TEXT, root_sha TEXT,
  frozen INTEGER NOT NULL DEFAULT 0,
  -- Push cursor (git-bridge §5.2): the last commit sha we pushed and the effective
  -- frontier it represents (JSON version vector). A plan whose effective frontier is
  -- covered by `last_pushed_frontier` is already pushed; the rest synthesize on top.
  last_pushed_sha TEXT, last_pushed_frontier TEXT
);
-- Derived cache of the ledger's mode/symlink/gitlink table (git-bridge §3.3/§6.3):
-- ASP doesn't model the +x bit, so push synthesis replays these. Rebuildable from
-- the GitIngest rows in the fold; node-private, like git_blobs.
CREATE TABLE IF NOT EXISTS git_modes(path TEXT PRIMARY KEY, mode INTEGER NOT NULL, kind TEXT NOT NULL DEFAULT 'file');
"#;

/// The `log` table's SECONDARY indexes, as a single source of truth. Each string
/// is byte-identical to how the index is first created (SCHEMA for `log_file`/
/// `log_site`; `migrate_branching` for the three branch/tag ones) — SQLite stores
/// the CREATE text verbatim in `sqlite_master.sql`, so [`SqliteStore::bulk_load`]
/// can drop these and recreate them with the EXACT same definitions. The
/// `bulk_load_rebuilds_the_exact_index_set` test pins that equality against a
/// freshly-opened store, so a definition can never silently drift here.
///
/// NOTE: this deliberately excludes the `id` PRIMARY KEY and the
/// `UNIQUE(site_id, seq)` constraint index — both are identity/dedup-bearing and
/// are kept during the bulk load (a pristine clone has no dups, so maintaining the
/// PK costs almost nothing).
const LOG_SECONDARY_INDEXES: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS log_file ON log(file_id)",
    "CREATE INDEX IF NOT EXISTS log_site ON log(site_id, seq)",
    "CREATE INDEX IF NOT EXISTS log_branch ON log(branch_id)",
    "CREATE INDEX IF NOT EXISTS log_kind_branch ON log(kind) WHERE kind='branch'",
    "CREATE INDEX IF NOT EXISTS log_kind_tag ON log(kind) WHERE kind='tag'",
];

/// The `log` secondary index names (for `DROP INDEX`), in the same order as
/// [`LOG_SECONDARY_INDEXES`].
const LOG_SECONDARY_INDEX_NAMES: &[&str] =
    &["log_file", "log_site", "log_branch", "log_kind_branch", "log_kind_tag"];

pub struct SqliteStore {
    conn: Connection,
}

/// A row of the node-private `git_remotes` table (git-bridge §4.1/§6.3). Holds the
/// per-remote bridge config + ingest cursor; never synced (URLs/credentials/cursor
/// are node-local).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRemoteRow {
    /// `remote_id(url)` — stable 16-hex id derived from the normalized URL.
    pub remote_id: String,
    /// The remote URL (no embedded credentials — tokens live in the keyring, §8).
    pub url: String,
    /// The ref push synthesis targets (default: the remote default branch). `None`
    /// until a push slice sets it.
    pub push_ref: Option<String>,
    /// Rollup policy (`manual` in v1).
    pub policy: String,
    /// The keyring entry name holding this remote's token, if any (§8).
    pub auth_ref: Option<String>,
    /// The remote's default branch short name (e.g. `main`).
    pub default_branch: Option<String>,
    /// The last upstream commit sha ingested into the vault (the pull cursor).
    pub last_ingested_sha: Option<String>,
    /// The full remote ref the cursor tracks (e.g. `refs/heads/main`).
    pub remote_ref: Option<String>,
    /// The imported main-chain root sha — the key for the derived repo `site_id`
    /// (git-bridge §3.2), needed to author ongoing-pull rows under the same site.
    pub root_sha: Option<String>,
    /// Set after an upstream force-push is detected; cleared by `rebaseline` (§4.4).
    pub frozen: bool,
    /// The last commit sha this node pushed to the remote (git-bridge §5.2), or `None`
    /// before the first push. Used as the push base and for idempotent-race detection.
    pub last_pushed_sha: Option<String>,
    /// The effective frontier (JSON version vector) the last-pushed tip represents.
    /// A plan whose effective frontier is covered by this is already pushed.
    pub last_pushed_frontier: Option<String>,
}

impl SqliteStore {
    pub fn open(path: &Path) -> AspResult<SqliteStore> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_memory() -> AspResult<SqliteStore> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> AspResult<SqliteStore> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;",
        )?;
        conn.execute_batch(SCHEMA)?;
        let store = SqliteStore { conn };
        store.migrate_branching()?;
        store.migrate_git_push()?;
        Ok(store)
    }

    /// Idempotent migration to the branching schema (§9): add the `branch_id`/
    /// `merge_parent` columns to a `log` table created before branching. New DBs
    /// already have them (the `CREATE TABLE` above), so the `ALTER`s are guarded by
    /// a column-existence check and skipped. Existing rows take the `'main'`
    /// default, reading back byte-identical to today.
    fn migrate_branching(&self) -> AspResult<()> {
        let have: std::collections::HashSet<String> = {
            let mut stmt = self.conn.prepare("PRAGMA table_info(log)")?;
            let cols = stmt.query_map([], |r| r.get::<_, String>(1))?;
            cols.collect::<Result<_, _>>()?
        };
        if !have.contains("branch_id") {
            self.conn.execute_batch("ALTER TABLE log ADD COLUMN branch_id TEXT NOT NULL DEFAULT 'main'")?;
        }
        if !have.contains("merge_parent") {
            self.conn.execute_batch("ALTER TABLE log ADD COLUMN merge_parent TEXT")?;
        }
        // Index on branch_id — created here (not in SCHEMA) so it runs only once the
        // column is guaranteed to exist, on both fresh and migrated DBs. Idempotent.
        self.conn.execute_batch("CREATE INDEX IF NOT EXISTS log_branch ON log(branch_id)")?;
        // Partial index over just the (few) `Kind::Branch` records, so
        // `branch_rows()`'s `WHERE kind='branch'` — re-run on every branch authoring
        // and remote integration by `reconcile_branches` — is a tiny index probe
        // instead of a full `log` scan. `kind` predates branching, so this is safe
        // on pre-branching DBs too. Idempotent.
        self.conn.execute_batch("CREATE INDEX IF NOT EXISTS log_kind_branch ON log(kind) WHERE kind='branch'")?;
        // Same discipline for tag records: a partial index so `tag_rows()` is a tiny
        // probe rather than a full `log` scan. Idempotent, safe on pre-tag DBs.
        self.conn.execute_batch("CREATE INDEX IF NOT EXISTS log_kind_tag ON log(kind) WHERE kind='tag'")?;
        Ok(())
    }

    /// Idempotent migration adding the push cursor columns to `git_remotes` created
    /// before the push slice (git-bridge §5.2). New DBs already have them.
    fn migrate_git_push(&self) -> AspResult<()> {
        let have: std::collections::HashSet<String> = {
            let mut stmt = self.conn.prepare("PRAGMA table_info(git_remotes)")?;
            let cols = stmt.query_map([], |r| r.get::<_, String>(1))?;
            cols.collect::<Result<_, _>>()?
        };
        if !have.contains("last_pushed_sha") {
            self.conn.execute_batch("ALTER TABLE git_remotes ADD COLUMN last_pushed_sha TEXT")?;
        }
        if !have.contains("last_pushed_frontier") {
            self.conn.execute_batch("ALTER TABLE git_remotes ADD COLUMN last_pushed_frontier TEXT")?;
        }
        Ok(())
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    // ----- log -----

    /// Append a row idempotently (dedup by Merkle id). Returns true if newly added.
    pub fn append_row(&self, row: &LogRow) -> AspResult<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO log(id, site_id, lamport, seq, ts, file_id, kind, merge_class, parent, base_hash, result_hash, path, branch_id, merge_parent, sig)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            params![
                row.id, row.site_id, row.lamport, row.seq, row.ts, row.file_id,
                row.kind.as_str(), row.merge_class.as_str(), row.parent, row.base_hash,
                row.result_hash, row.path, row.branch_id, row.merge_parent,
                if row.sig.is_empty() { None } else { Some(&row.sig) }
            ],
        )?;
        Ok(n > 0)
    }

    /// Append many rows idempotently (dedup by Merkle id) in ONE transaction,
    /// returning a per-row flag (true = newly added). Mirrors `replace_files`'
    /// single-transaction + cached-statement pattern: without it every INSERT
    /// auto-commits its own WAL transaction, and at ~41k rows that per-row commit
    /// overhead dominated a large git clone's "saving" phase. `unchecked_transaction`
    /// is safe for the same reason it is there — the engine is single-threaded
    /// behind its `Mutex`, so there is never a nested/concurrent transaction here.
    pub fn append_rows<'a>(&self, rows: impl IntoIterator<Item = &'a LogRow>) -> AspResult<Vec<bool>> {
        let tx = self.conn.unchecked_transaction()?;
        let mut flags = Vec::new();
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR IGNORE INTO log(id, site_id, lamport, seq, ts, file_id, kind, merge_class, parent, base_hash, result_hash, path, branch_id, merge_parent, sig)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            )?;
            for row in rows {
                let n = stmt.execute(params![
                    row.id, row.site_id, row.lamport, row.seq, row.ts, row.file_id,
                    row.kind.as_str(), row.merge_class.as_str(), row.parent, row.base_hash,
                    row.result_hash, row.path, row.branch_id, row.merge_parent,
                    if row.sig.is_empty() { None } else { Some(&row.sig) }
                ])?;
                flags.push(n > 0);
            }
        }
        tx.commit()?;
        Ok(flags)
    }

    /// Insert many content-addressed blobs in ONE transaction, verifying each
    /// blob's bytes hash to its declared `hash` (the integrity check `integrate`
    /// does per blob — preserved here, just batched). Same transaction rationale
    /// as [`append_rows`](Self::append_rows). Errors `blob hash mismatch` on the
    /// first blob whose bytes don't match, rolling the batch back.
    pub fn put_blobs<'a>(&self, blobs: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> AspResult<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt =
                tx.prepare_cached("INSERT OR IGNORE INTO blobs(content_hash, bytes) VALUES (?1, ?2)")?;
            for (hash, bytes) in blobs {
                if crate::oid::content_hash(bytes) != hash {
                    return Err(crate::error::AspError::Protocol("blob hash mismatch".into()));
                }
                stmt.execute(params![hash, bytes])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Run `f` as a **pristine-clone bulk load**: drop the `log` table's SECONDARY
    /// indexes and switch to bulk-load PRAGMAs first, then rebuild the exact same
    /// indexes and restore the PRAGMAs afterward. Building each index once over the
    /// finished N-row table is far cheaper than N incremental b-tree updates (five
    /// secondary indexes per INSERT dominated a big clone's insert phase). Returns
    /// `f`'s value.
    ///
    /// SAFETY — only for the pristine git-clone path:
    /// - Single-threaded: `Engine` is `!Sync` and only ever touched behind its
    ///   `Mutex`, so nothing reads the (index-less) `log` concurrently. The one
    ///   thing that *would* query a dropped index mid-load — `reconcile_branches`
    ///   (via the `log_kind_branch` partial index) — is deferred by the clone
    ///   driver (`Engine::set_bulk`) until after this returns and the index is back.
    /// - Durability: a clone is all-or-nothing (git-bridge §9) — a torn clone's
    ///   half-built vault is discarded — so `synchronous=OFF` strictly during the
    ///   bulk insert is safe. It is restored to `NORMAL` here before returning.
    /// - The indexes are rebuilt and PRAGMAs restored even if `f` returns `Err`, so
    ///   a *caught* clone error still leaves a schema-consistent db. (`f`'s error
    ///   takes precedence over a rebuild error in the return.)
    pub fn bulk_load<T>(&self, f: impl FnOnce() -> AspResult<T>) -> AspResult<T> {
        self.begin_bulk_load()?;
        let out = f();
        let finish = self.end_bulk_load();
        // `f`'s error dominates (the indexes are already rebuilt regardless); a
        // rebuild/restore error only surfaces if `f` itself succeeded.
        match out {
            Ok(v) => finish.map(|()| v),
            Err(e) => Err(e),
        }
    }

    /// Apply the bulk-load durability PRAGMAs **without** touching indexes — used by the
    /// pristine git clone to cover its pack-decode blob spill (which streams the repo's
    /// blobs onto disk before the `bulk_load` insert window). `synchronous=OFF` is safe
    /// for the same reason as [`bulk_load`](Self::bulk_load): a torn clone is discarded.
    /// The matching `end_bulk_load` (run after integrate) restores `synchronous=NORMAL`.
    pub fn set_bulk_pragmas(&self) -> AspResult<()> {
        self.conn.execute_batch(
            "PRAGMA synchronous=OFF; PRAGMA temp_store=MEMORY; PRAGMA cache_size=-262144; PRAGMA mmap_size=1073741824;",
        )?;
        Ok(())
    }

    /// Drop the secondary indexes + apply bulk-load PRAGMAs. See [`bulk_load`](Self::bulk_load).
    fn begin_bulk_load(&self) -> AspResult<()> {
        // `synchronous=OFF` (safe: a failed clone is discarded) + more scratch/cache
        // room for the index rebuild. `cache_size` negative = KiB; `mmap_size` bytes.
        self.conn.execute_batch(
            "PRAGMA synchronous=OFF; PRAGMA temp_store=MEMORY; PRAGMA cache_size=-262144; PRAGMA mmap_size=1073741824;",
        )?;
        for name in LOG_SECONDARY_INDEX_NAMES {
            self.conn.execute_batch(&format!("DROP INDEX IF EXISTS {name}"))?;
        }
        Ok(())
    }

    /// Rebuild the exact secondary index set + restore durability. See [`bulk_load`](Self::bulk_load).
    fn end_bulk_load(&self) -> AspResult<()> {
        for stmt in LOG_SECONDARY_INDEXES {
            self.conn.execute_batch(stmt)?;
        }
        self.conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
        Ok(())
    }

    pub fn has_row(&self, id: &str) -> AspResult<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM log WHERE id=?1", params![id], |_| Ok(()))
            .optional()?
            .is_some())
    }

    fn row_from(r: &rusqlite::Row) -> rusqlite::Result<LogRow> {
        let sig: Option<Vec<u8>> = r.get("sig")?;
        Ok(LogRow {
            id: r.get("id")?,
            site_id: r.get("site_id")?,
            lamport: r.get::<_, i64>("lamport")? as u64,
            seq: r.get::<_, i64>("seq")? as u64,
            ts: r.get("ts")?,
            file_id: r.get("file_id")?,
            kind: Kind::parse(&r.get::<_, String>("kind")?).unwrap_or(Kind::Edit),
            merge_class: MergeClass::parse(&r.get::<_, String>("merge_class")?).unwrap_or(MergeClass::Text),
            parent: r.get("parent")?,
            base_hash: r.get("base_hash")?,
            result_hash: r.get("result_hash")?,
            path: r.get("path")?,
            branch_id: r.get("branch_id")?,
            merge_parent: r.get("merge_parent")?,
            sig: sig.unwrap_or_default(),
        })
    }

    pub fn all_rows(&self) -> AspResult<Vec<LogRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT * FROM log ORDER BY lamport, site_id, id",
        )?;
        let rows = stmt.query_map([], Self::row_from)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Every `Kind::Branch` record row (§7) — the synced branch metadata, which
    /// `reconcile_branches` folds (LWW) into the branch set. Cheap: branch records
    /// are few relative to content rows.
    pub fn branch_rows(&self) -> AspResult<Vec<LogRow>> {
        let mut stmt = self.conn.prepare("SELECT * FROM log WHERE kind='branch'")?;
        let rows = stmt.query_map([], Self::row_from)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Every `Kind::Tag` record row — the synced tag metadata, which `reconcile_tags`
    /// folds (LWW) into the tag set. Cheap: uses the `log_kind_tag` partial index.
    pub fn tag_rows(&self) -> AspResult<Vec<LogRow>> {
        let mut stmt = self.conn.prepare("SELECT * FROM log WHERE kind='tag'")?;
        let rows = stmt.query_map([], Self::row_from)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Every row for one `file_id` (uses the `log_file` index) — the incremental
    /// fold re-folds a touched file from exactly these. Order is irrelevant;
    /// `fold_order` re-sorts.
    pub fn rows_for_file(&self, file_id: &str) -> AspResult<Vec<LogRow>> {
        let mut stmt = self.conn.prepare("SELECT * FROM log WHERE file_id=?1")?;
        let rows = stmt.query_map(params![file_id], Self::row_from)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Max Lamport across the log (0 if empty) — the derived-git commit time.
    pub fn max_lamport(&self) -> AspResult<u64> {
        let m: i64 = self.conn.query_row("SELECT COALESCE(MAX(lamport),0) FROM log", [], |r| r.get(0))?;
        Ok(m as u64)
    }

    /// Rows authored by `site` with `seq > after`, ascending — what a peer is
    /// missing per the version vector.
    pub fn rows_after(&self, site: &str, after: i64) -> AspResult<Vec<LogRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM log WHERE site_id=?1 AND seq>?2 ORDER BY seq")?;
        let rows = stmt.query_map(params![site, after], Self::row_from)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// One page of a site's rows after `after` (ascending), capped at `limit` —
    /// the cursor for streaming a large catch-up without loading the whole site
    /// (and its blobs) into memory at once. See `Step::CatchUp`.
    pub fn rows_after_page(&self, site: &str, after: i64, limit: i64) -> AspResult<Vec<LogRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM log WHERE site_id=?1 AND seq>?2 ORDER BY seq LIMIT ?3")?;
        let rows = stmt.query_map(params![site, after, limit], Self::row_from)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Version vector across all known devices: site_id -> max seq held.
    pub fn version_vector(&self) -> AspResult<BTreeMap<String, i64>> {
        let mut stmt = self.conn.prepare("SELECT site_id, MAX(seq) FROM log GROUP BY site_id")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
        let mut vv = BTreeMap::new();
        for r in rows {
            let (s, m) = r?;
            vv.insert(s, m);
        }
        Ok(vv)
    }

    pub fn row_count(&self) -> AspResult<u64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM log", [], |r| r.get::<_, i64>(0))? as u64)
    }

    /// Wall-clock timestamp of the most recent log row (for "last synced"), or
    /// `None` for an empty log. A single aggregate — never load every row just
    /// to take a max (the status poll runs periodically on the active vault).
    pub fn max_ts(&self) -> AspResult<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT MAX(ts) FROM log", [], |r| r.get::<_, Option<i64>>(0))?)
    }

    /// Count of live (non-tombstone) materialized files — a single aggregate, so
    /// the status poll never materializes every `FileRow` just to count them
    /// (must stay O(1) on a big vault).
    pub fn live_file_count(&self) -> AspResult<usize> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM files WHERE deleted=0", [], |r| r.get::<_, i64>(0))? as usize)
    }

    /// The live (non-tombstone) file at `path`, if any — an indexed lookup, not a
    /// load of every file row (record_* run it on every edit).
    pub fn live_file_by_path(&self, path: &str) -> AspResult<Option<FileRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT file_id, path, result_hash, merge_class, deleted, lamport, site_id, conflict
             FROM files WHERE path=?1 AND deleted=0 LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![path], |r| {
            Ok(FileRow {
                file_id: r.get(0)?,
                path: r.get(1)?,
                result_hash: r.get(2)?,
                merge_class: MergeClass::parse(&r.get::<_, String>(3)?).unwrap_or(MergeClass::Text),
                deleted: r.get::<_, i64>(4)? != 0,
                lamport: r.get::<_, i64>(5)? as u64,
                site_id: r.get(6)?,
                conflict: r.get::<_, i64>(7)? != 0,
            })
        })?;
        Ok(rows.next().transpose()?)
    }

    /// Derived-git blob-object id for a content hash, if already exported. The git
    /// object id is a pure function of the bytes, so this cache lets `materialize`
    /// skip re-reading and re-hashing every blob into the git store on every
    /// settle — only content first seen this session is read + written.
    pub fn git_oid_for(&self, content_hash: &str) -> AspResult<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT git_oid FROM git_blobs WHERE content_hash=?1", params![content_hash], |r| r.get::<_, String>(0))
            .optional()?)
    }

    pub fn put_git_oid(&self, content_hash: &str, git_oid: &str) -> AspResult<()> {
        self.conn.execute(
            "INSERT INTO git_blobs(content_hash, git_oid) VALUES(?1,?2) ON CONFLICT(content_hash) DO NOTHING",
            params![content_hash, git_oid],
        )?;
        Ok(())
    }

    /// Batch-insert `(content_hash, git_oid)` pairs in ONE transaction — the derived
    /// git export computes every blob's oid in parallel then persists them here in
    /// bulk (far cheaper than N autocommitted inserts on a fresh clone's 41k blobs).
    pub fn put_git_oids<'a>(&self, pairs: impl IntoIterator<Item = (&'a str, &'a str)>) -> AspResult<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO git_blobs(content_hash, git_oid) VALUES(?1,?2) ON CONFLICT(content_hash) DO NOTHING",
            )?;
            for (content_hash, git_oid) in pairs {
                stmt.execute(params![content_hash, git_oid])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Next Lamport tick = max(observed) + 1, derived from the durable log.
    pub fn next_lamport(&self, observed: u64) -> AspResult<u64> {
        let max_log: i64 = self
            .conn
            .query_row("SELECT COALESCE(MAX(lamport),0) FROM log", [], |r| r.get(0))?;
        Ok(std::cmp::max(max_log as u64, observed) + 1)
    }

    /// Next dense `seq` for the local device.
    pub fn next_seq(&self, site: &str) -> AspResult<u64> {
        let max_seq: Option<i64> = self
            .conn
            .query_row("SELECT MAX(seq) FROM log WHERE site_id=?1", params![site], |r| r.get(0))
            .optional()?
            .flatten();
        Ok(match max_seq {
            Some(s) => (s + 1) as u64,
            None => 0,
        })
    }

    // ----- materialized files -----

    /// Reconcile the `files` table to `files` by DELTA: read the current rows,
    /// upsert only the file_ids whose row actually changed, and delete any that
    /// disappeared. Same end state as `replace_files`, but a single-file change
    /// writes one row instead of rewriting the whole table — turning the per-op
    /// files-table cost from O(vault) writes (DELETE all + reinsert all, which
    /// also churns the WAL) into O(changed) writes + one O(vault) read. The reads
    /// are far cheaper than the writes/fsync they replace.
    pub fn sync_files(&self, files: &[FileRow]) -> AspResult<()> {
        // Current table state keyed by file_id (includes tombstones).
        let mut old: std::collections::HashMap<String, FileRow> = std::collections::HashMap::new();
        {
            let mut stmt = self.conn.prepare(
                "SELECT file_id, path, result_hash, merge_class, deleted, lamport, site_id, conflict FROM files",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(FileRow {
                    file_id: r.get(0)?,
                    path: r.get(1)?,
                    result_hash: r.get(2)?,
                    merge_class: MergeClass::parse(&r.get::<_, String>(3)?).unwrap_or(MergeClass::Text),
                    deleted: r.get::<_, i64>(4)? != 0,
                    lamport: r.get::<_, i64>(5)? as u64,
                    site_id: r.get(6)?,
                    conflict: r.get::<_, i64>(7)? != 0,
                })
            })?;
            for r in rows {
                let f = r?;
                old.insert(f.file_id.clone(), f);
            }
        }
        let new_ids: std::collections::HashSet<&str> = files.iter().map(|f| f.file_id.as_str()).collect();
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut up = tx.prepare_cached(
                "INSERT INTO files(file_id, path, result_hash, merge_class, deleted, lamport, site_id, conflict)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
                 ON CONFLICT(file_id) DO UPDATE SET
                   path=?2, result_hash=?3, merge_class=?4, deleted=?5, lamport=?6, site_id=?7, conflict=?8",
            )?;
            for f in files {
                if old.get(&f.file_id) == Some(f) {
                    continue; // row unchanged — no write
                }
                up.execute(params![
                    f.file_id, f.path, f.result_hash, f.merge_class.as_str(),
                    f.deleted as i64, f.lamport as i64, f.site_id, f.conflict as i64
                ])?;
            }
            let mut del = tx.prepare_cached("DELETE FROM files WHERE file_id=?1")?;
            for id in old.keys() {
                if !new_ids.contains(id.as_str()) {
                    del.execute(params![id])?;
                }
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn replace_files(&self, files: &[FileRow]) -> AspResult<()> {
        // ONE transaction for the whole rewrite. Without it, every INSERT
        // auto-commits its own WAL transaction — at 10k+ files that per-row
        // commit overhead dominated `materialize` (≈585ms of an 824ms write at
        // 10k files), making every save O(vault) in commits. A single
        // transaction + a cached prepared statement collapses that to one
        // commit. `unchecked_transaction` is safe here: `replace_files` is only
        // called inside `materialize`, which holds the engine lock, so there is
        // never a nested/concurrent transaction on this connection.
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM files", [])?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT INTO files(file_id, path, result_hash, merge_class, deleted, lamport, site_id, conflict)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            )?;
            for f in files {
                stmt.execute(params![
                    f.file_id, f.path, f.result_hash, f.merge_class.as_str(),
                    f.deleted as i64, f.lamport as i64, f.site_id, f.conflict as i64
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Update just one file's content hash (+ the authoring clock) in place — the
    /// incremental-materialize fast path for a local linear edit, which changes
    /// exactly one file's content and nothing structural (path/class/deleted/
    /// conflict are untouched, matching what a full fold would leave them).
    pub fn update_file_hash(&self, file_id: &str, result_hash: &str, lamport: u64, site_id: &str) -> AspResult<()> {
        self.conn.execute(
            "UPDATE files SET result_hash=?2, lamport=?3, site_id=?4 WHERE file_id=?1",
            params![file_id, result_hash, lamport as i64, site_id],
        )?;
        Ok(())
    }

    /// Every file row, tombstones included — the prior fold result, for diffing a
    /// fresh fold so materialize only touches what changed (vs `replace_files`,
    /// which rewrites the whole table on every edit — O(N) per keystroke).
    pub fn all_files(&self) -> AspResult<Vec<FileRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT file_id, path, result_hash, merge_class, deleted, lamport, site_id, conflict FROM files",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(FileRow {
                    file_id: r.get(0)?,
                    path: r.get(1)?,
                    result_hash: r.get(2)?,
                    merge_class: MergeClass::parse(&r.get::<_, String>(3)?).unwrap_or(MergeClass::Text),
                    deleted: r.get::<_, i64>(4)? != 0,
                    lamport: r.get::<_, i64>(5)? as u64,
                    site_id: r.get(6)?,
                    conflict: r.get::<_, i64>(7)? != 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Delete specific file rows by `file_id` — for fold outputs that drop a row
    /// entirely (a deleted directory entity gets no tombstone, unlike a content
    /// file), which `replace_files` used to handle by rewriting the whole table.
    pub fn delete_file_rows(&self, file_ids: &[String]) -> AspResult<()> {
        let tx = self.conn.unchecked_transaction()?;
        for id in file_ids {
            tx.execute("DELETE FROM files WHERE file_id = ?1", params![id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Load the (mtime_ns, size, content_hash) reconcile cache: path -> stat+hash.
    pub fn load_fs_stat(&self) -> AspResult<std::collections::HashMap<String, (i64, i64, String)>> {
        let mut stmt = self.conn.prepare("SELECT path, mtime_ns, size, hash FROM fs_stat")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?)))
        })?;
        Ok(rows.collect::<Result<std::collections::HashMap<_, _>, _>>()?)
    }

    /// Record the stat+hash for files (re)read during a scan (one txn = one fsync).
    pub fn upsert_fs_stat(&self, entries: &[(String, i64, i64, String)]) -> AspResult<()> {
        if entries.is_empty() {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        for (path, mtime_ns, size, hash) in entries {
            tx.execute(
                "INSERT OR REPLACE INTO fs_stat(path, mtime_ns, size, hash) VALUES (?1,?2,?3,?4)",
                params![path, mtime_ns, size, hash],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Drop reconcile-cache entries for paths no longer on disk (kept in sync).
    pub fn delete_fs_stat(&self, paths: &[String]) -> AspResult<()> {
        if paths.is_empty() {
            return Ok(());
        }
        let tx = self.conn.unchecked_transaction()?;
        for p in paths {
            tx.execute("DELETE FROM fs_stat WHERE path = ?1", params![p])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Insert-or-replace specific file rows by `file_id` (the PK) — the
    /// incremental counterpart to `replace_files`. One transaction = one fsync.
    pub fn upsert_files(&self, files: &[FileRow]) -> AspResult<()> {
        let tx = self.conn.unchecked_transaction()?;
        for f in files {
            tx.execute(
                "INSERT OR REPLACE INTO files(file_id, path, result_hash, merge_class, deleted, lamport, site_id, conflict)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    f.file_id, f.path, f.result_hash, f.merge_class.as_str(),
                    f.deleted as i64, f.lamport as i64, f.site_id, f.conflict as i64
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn live_files(&self) -> AspResult<Vec<FileRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT file_id, path, result_hash, merge_class, deleted, lamport, site_id, conflict
             FROM files WHERE deleted=0 ORDER BY path",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(FileRow {
                file_id: r.get(0)?,
                path: r.get(1)?,
                result_hash: r.get(2)?,
                merge_class: MergeClass::parse(&r.get::<_, String>(3)?).unwrap_or(MergeClass::Text),
                deleted: r.get::<_, i64>(4)? != 0,
                lamport: r.get::<_, i64>(5)? as u64,
                site_id: r.get(6)?,
                conflict: r.get::<_, i64>(7)? != 0,
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// file_id for a live path (capture maps an FS event to its file_id).
    pub fn file_id_for_path(&self, path: &str) -> AspResult<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT file_id FROM files WHERE path=?1 AND deleted=0",
                params![path],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
    }

    // ----- branches / head (§2, §3.2) -----

    /// All branch records (excludes the implicit `main`, which the [`BranchSet`]
    /// injects). Soft-deleted branches are included — their rows still exist for
    /// sync/history; callers filter on `deleted` for the live set.
    pub fn branches(&self) -> AspResult<Vec<Branch>> {
        let mut stmt = self.conn.prepare(
            "SELECT branch_id, name, parent, fork_vv, created_lamport, created_ts, deleted FROM branches ORDER BY created_lamport, branch_id",
        )?;
        let rows = stmt.query_map([], Self::branch_from)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    fn branch_from(r: &rusqlite::Row) -> rusqlite::Result<Branch> {
        let fork_vv: String = r.get("fork_vv")?;
        Ok(Branch {
            branch_id: r.get("branch_id")?,
            name: r.get("name")?,
            parent: r.get("parent")?,
            fork_vv: serde_json::from_str(&fork_vv).unwrap_or_default(),
            created_lamport: r.get::<_, i64>("created_lamport")? as u64,
            created_ts: r.get("created_ts")?,
            deleted: r.get::<_, i64>("deleted")? != 0,
        })
    }

    /// Upsert a branch record (last-writer-wins on name/deleted is resolved by the
    /// caller in P4; here it just persists the latest known state).
    pub fn put_branch(&self, b: &Branch) -> AspResult<()> {
        let fork_vv = serde_json::to_string(&b.fork_vv).unwrap_or_else(|_| "{}".into());
        self.conn.execute(
            "INSERT INTO branches(branch_id, name, parent, fork_vv, created_lamport, created_ts, deleted)
             VALUES(?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(branch_id) DO UPDATE SET name=?2, parent=?3, fork_vv=?4, created_lamport=?5, created_ts=?6, deleted=?7",
            params![b.branch_id, b.name, b.parent, fork_vv, b.created_lamport as i64, b.created_ts, b.deleted as i64],
        )?;
        Ok(())
    }

    pub fn branch(&self, id: &str) -> AspResult<Option<Branch>> {
        if id == crate::log::MAIN_BRANCH_ID {
            return Ok(Some(Branch::main()));
        }
        Ok(self
            .conn
            .query_row(
                "SELECT branch_id, name, parent, fork_vv, created_lamport, created_ts, deleted FROM branches WHERE branch_id=?1",
                params![id],
                Self::branch_from,
            )
            .optional()?)
    }

    /// The checked-out branch (HEAD). Defaults to `main` when unset (a fresh or
    /// pre-branching vault), so a single-branch vault folds `main` exactly as today.
    pub fn head(&self) -> AspResult<String> {
        Ok(self
            .conn
            .query_row("SELECT branch_id FROM head WHERE singleton=0", [], |r| r.get::<_, String>(0))
            .optional()?
            .unwrap_or_else(|| crate::log::MAIN_BRANCH_ID.to_string()))
    }

    pub fn set_head(&self, branch_id: &str) -> AspResult<()> {
        self.conn.execute(
            "INSERT INTO head(singleton, branch_id) VALUES(0, ?1) ON CONFLICT(singleton) DO UPDATE SET branch_id=?1",
            params![branch_id],
        )?;
        Ok(())
    }

    // ----- config -----

    pub fn set_config(&self, key: &str, value: &str) -> AspResult<()> {
        self.conn.execute(
            "INSERT INTO config(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=?2",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_config(&self, key: &str) -> AspResult<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM config WHERE key=?1", params![key], |r| r.get::<_, String>(0))
            .optional()?)
    }

    // ----- git remotes (git-bridge §4.1/§6.3, node-private) -----

    /// Insert or replace a git-remote config row (keyed by `remote_id`).
    pub fn git_remote_upsert(&self, r: &GitRemoteRow) -> AspResult<()> {
        self.conn.execute(
            "INSERT INTO git_remotes(remote_id, url, push_ref, policy, auth_ref, default_branch, last_ingested_sha, remote_ref, root_sha, frozen, last_pushed_sha, last_pushed_frontier)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)
             ON CONFLICT(remote_id) DO UPDATE SET
               url=?2, push_ref=?3, policy=?4, auth_ref=?5, default_branch=?6,
               last_ingested_sha=?7, remote_ref=?8, root_sha=?9, frozen=?10,
               last_pushed_sha=?11, last_pushed_frontier=?12",
            params![
                r.remote_id, r.url, r.push_ref, r.policy, r.auth_ref, r.default_branch,
                r.last_ingested_sha, r.remote_ref, r.root_sha, r.frozen as i64,
                r.last_pushed_sha, r.last_pushed_frontier
            ],
        )?;
        Ok(())
    }

    fn git_remote_from(row: &rusqlite::Row) -> rusqlite::Result<GitRemoteRow> {
        Ok(GitRemoteRow {
            remote_id: row.get(0)?,
            url: row.get(1)?,
            push_ref: row.get(2)?,
            policy: row.get(3)?,
            auth_ref: row.get(4)?,
            default_branch: row.get(5)?,
            last_ingested_sha: row.get(6)?,
            remote_ref: row.get(7)?,
            root_sha: row.get(8)?,
            frozen: row.get::<_, i64>(9)? != 0,
            last_pushed_sha: row.get(10)?,
            last_pushed_frontier: row.get(11)?,
        })
    }

    /// The git-remote config for `remote_id`, if any.
    pub fn git_remote_get(&self, remote_id: &str) -> AspResult<Option<GitRemoteRow>> {
        Ok(self
            .conn
            .query_row(
                "SELECT remote_id, url, push_ref, policy, auth_ref, default_branch, last_ingested_sha, remote_ref, root_sha, frozen, last_pushed_sha, last_pushed_frontier
                 FROM git_remotes WHERE remote_id=?1",
                params![remote_id],
                Self::git_remote_from,
            )
            .optional()?)
    }

    /// Every configured git remote (creation order is not preserved — sorted by id).
    pub fn git_remote_list(&self) -> AspResult<Vec<GitRemoteRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT remote_id, url, push_ref, policy, auth_ref, default_branch, last_ingested_sha, remote_ref, root_sha, frozen, last_pushed_sha, last_pushed_frontier
             FROM git_remotes ORDER BY remote_id",
        )?;
        let rows = stmt.query_map([], Self::git_remote_from)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Remove a git-remote config row.
    pub fn git_remote_remove(&self, remote_id: &str) -> AspResult<()> {
        self.conn.execute("DELETE FROM git_remotes WHERE remote_id=?1", params![remote_id])?;
        Ok(())
    }

    /// Set/clear the frozen (force-push detected) flag (git-bridge §4.4).
    pub fn git_remote_set_frozen(&self, remote_id: &str, frozen: bool) -> AspResult<()> {
        self.conn.execute(
            "UPDATE git_remotes SET frozen=?2 WHERE remote_id=?1",
            params![remote_id, frozen as i64],
        )?;
        Ok(())
    }

    /// Advance the ingest cursor after a successful pull (git-bridge §4.2).
    pub fn git_remote_set_ingested(&self, remote_id: &str, sha: &str, remote_ref: &str) -> AspResult<()> {
        self.conn.execute(
            "UPDATE git_remotes SET last_ingested_sha=?2, remote_ref=?3 WHERE remote_id=?1",
            params![remote_id, sha, remote_ref],
        )?;
        Ok(())
    }

    /// Advance the push cursor after a successful push (git-bridge §5.2): the tip sha
    /// and the JSON-encoded effective frontier it represents.
    pub fn git_remote_set_pushed(&self, remote_id: &str, sha: &str, frontier_json: &str) -> AspResult<()> {
        self.conn.execute(
            "UPDATE git_remotes SET last_pushed_sha=?2, last_pushed_frontier=?3 WHERE remote_id=?1",
            params![remote_id, sha, frontier_json],
        )?;
        Ok(())
    }

    // ----- git modes (derived mode/symlink/gitlink cache) -----

    /// Record one path's git mode + kind (`file`/`symlink`/`gitlink`).
    pub fn git_mode_put(&self, path: &str, mode: u32, kind: &str) -> AspResult<()> {
        self.conn.execute(
            "INSERT INTO git_modes(path, mode, kind) VALUES(?1,?2,?3)
             ON CONFLICT(path) DO UPDATE SET mode=?2, kind=?3",
            params![path, mode as i64, kind],
        )?;
        Ok(())
    }

    /// Every recorded `(path, mode, kind)`.
    pub fn git_mode_get_all(&self) -> AspResult<Vec<(String, u32, String)>> {
        let mut stmt = self.conn.prepare("SELECT path, mode, kind FROM git_modes ORDER BY path")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u32, r.get::<_, String>(2)?))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// Drop the whole mode cache (before a full rebuild from the ledger).
    pub fn git_mode_clear(&self) -> AspResult<()> {
        self.conn.execute("DELETE FROM git_modes", [])?;
        Ok(())
    }

    // ----- peers -----

    pub fn add_peer(&self, url: &str, node_id: &str, now: u64) -> AspResult<()> {
        self.conn.execute(
            "INSERT INTO peers(url,node_id,pinned_at) VALUES(?1,?2,?3)
             ON CONFLICT(url) DO UPDATE SET node_id=?2",
            params![url, node_id, now as i64],
        )?;
        Ok(())
    }

    pub fn peers(&self) -> AspResult<Vec<(String, String)>> {
        let mut stmt = self.conn.prepare("SELECT url, node_id FROM peers ORDER BY url")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    // ----- authorized_keys -----

    pub fn insert_authkey(&self, k: &AuthKey) -> AspResult<()> {
        self.conn.execute(
            "INSERT INTO authorized_keys(ssh_pubkey, node_id, expires_at, never, added_at, source)
             VALUES(?1,?2,?3,?4,?5,?6)
             ON CONFLICT(ssh_pubkey) DO UPDATE SET expires_at=?3, never=?4, source=?6",
            params![
                k.ssh_pubkey, k.node_id, k.expires_at.map(|x| x as i64),
                k.never as i64, k.added_at as i64, k.source
            ],
        )?;
        Ok(())
    }

    fn authkey_from(r: &rusqlite::Row) -> rusqlite::Result<AuthKey> {
        Ok(AuthKey {
            ssh_pubkey: r.get("ssh_pubkey")?,
            node_id: r.get("node_id")?,
            expires_at: r.get::<_, Option<i64>>("expires_at")?.map(|x| x as u64),
            never: r.get::<_, i64>("never")? != 0,
            added_at: r.get::<_, i64>("added_at")? as u64,
            source: r.get("source")?,
        })
    }

    pub fn authkeys(&self) -> AspResult<Vec<AuthKey>> {
        let mut stmt = self.conn.prepare("SELECT * FROM authorized_keys ORDER BY added_at, ssh_pubkey")?;
        let rows = stmt.query_map([], Self::authkey_from)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn authkey_by_node(&self, node_hex: &str) -> AspResult<Option<AuthKey>> {
        Ok(self
            .conn
            .query_row("SELECT * FROM authorized_keys WHERE node_id=?1", params![node_hex], Self::authkey_from)
            .optional()?)
    }

    pub fn delete_authkey_by_node(&self, node_hex: &str) -> AspResult<bool> {
        let n = self
            .conn
            .execute("DELETE FROM authorized_keys WHERE node_id=?1", params![node_hex])?;
        Ok(n > 0)
    }

    pub fn authkeys_empty(&self) -> AspResult<bool> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM authorized_keys", [], |r| r.get::<_, i64>(0))? == 0)
    }

    /// Listen-start migration: fill `expires_at` on any unset (NULL, never=0) row
    /// with `today + default_ttl`. Idempotent; leaves `never=1` and already-set
    /// rows untouched. Returns the number of rows filled.
    pub fn migrate_fill_expiry(&self, default_expiry: u64) -> AspResult<usize> {
        let n = self.conn.execute(
            "UPDATE authorized_keys SET expires_at=?1 WHERE expires_at IS NULL AND never=0",
            params![default_expiry as i64],
        )?;
        Ok(n)
    }

    pub fn set_authkey_expiry(&self, node_hex: &str, expires_at: Option<u64>, never: bool) -> AspResult<bool> {
        let n = self.conn.execute(
            "UPDATE authorized_keys SET expires_at=?1, never=?2 WHERE node_id=?3",
            params![expires_at.map(|x| x as i64), never as i64, node_hex],
        )?;
        Ok(n > 0)
    }

    // ----- snapshots -----

    pub fn insert_snapshot(
        &self,
        snapshot_id: &str,
        created_lamport: u64,
        label: &str,
        tree_hash: &str,
        created_ts: i64,
        manifest: &str,
    ) -> AspResult<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO snapshots(snapshot_id, created_lamport, label, tree_hash, created_ts, manifest)
             VALUES(?1,?2,?3,?4,?5,?6)",
            params![snapshot_id, created_lamport as i64, label, tree_hash, created_ts, manifest],
        )?;
        Ok(())
    }

    pub fn snapshot_by_label(&self, label: &str) -> AspResult<Option<(String, String, String)>> {
        // returns (snapshot_id, tree_hash, manifest)
        Ok(self
            .conn
            .query_row(
                "SELECT snapshot_id, tree_hash, manifest FROM snapshots WHERE label=?1 ORDER BY created_lamport DESC LIMIT 1",
                params![label],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)),
            )
            .optional()?)
    }

    pub fn snapshots(&self) -> AspResult<Vec<(String, String, u64)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT snapshot_id, label, created_lamport FROM snapshots ORDER BY created_lamport")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)? as u64))
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    // ----- embeddings (schema/API only in v1; never populated) -----

    pub fn put_embedding(&self, content_hash: &str, model_id: &str, vector: &[u8]) -> AspResult<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO embeddings(content_hash, model_id, vector) VALUES(?1,?2,?3)",
            params![content_hash, model_id, vector],
        )?;
        Ok(())
    }

    pub fn get_embedding(&self, content_hash: &str, model_id: &str) -> AspResult<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row(
                "SELECT vector FROM embeddings WHERE content_hash=?1 AND model_id=?2",
                params![content_hash, model_id],
                |r| r.get::<_, Vec<u8>>(0),
            )
            .optional()?)
    }
}

impl BlobStore for SqliteStore {
    fn put_blob(&self, bytes: &[u8]) -> AspResult<String> {
        let h = crate::oid::content_hash(bytes);
        self.conn.execute("INSERT OR IGNORE INTO blobs(content_hash, bytes) VALUES (?1, ?2)", params![h, bytes])?;
        Ok(h)
    }
    /// One transaction over the whole batch (like [`put_blobs`](Self::put_blobs)),
    /// **trusting** the caller-supplied content hash — the pack-decode spill already
    /// computed `content_hash(bytes)` for every blob, so re-hashing here would just
    /// re-pay the SHA-256 we moved off the decode path. See the trait method docs.
    fn put_blobs_with_hash_owned(&self, batch: Vec<(String, Vec<u8>)>) -> AspResult<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt =
                tx.prepare_cached("INSERT OR IGNORE INTO blobs(content_hash, bytes) VALUES (?1, ?2)")?;
            for (hash, bytes) in &batch {
                stmt.execute(params![hash, bytes])?;
            }
        }
        tx.commit()?;
        Ok(())
    }
    fn get_blob(&self, hash: &str) -> AspResult<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row("SELECT bytes FROM blobs WHERE content_hash=?1", params![hash], |r| r.get::<_, Vec<u8>>(0))
            .optional()?)
    }
    fn has_blob(&self, hash: &str) -> AspResult<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM blobs WHERE content_hash=?1", params![hash], |_| Ok(()))
            .optional()?
            .is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_and_log_roundtrip_with_counters() {
        let s = SqliteStore::open_memory().unwrap();
        let h = s.put_blob(b"hi").unwrap();
        assert_eq!(s.get_blob(&h).unwrap().as_deref(), Some(&b"hi"[..]));
        assert!(s.has_blob(&h).unwrap());
        // Counters derive from the durable log.
        assert_eq!(s.next_lamport(0).unwrap(), 1);
        assert_eq!(s.next_seq("aa").unwrap(), 0);
    }

    #[test]
    fn branches_and_head_roundtrip() {
        let s = SqliteStore::open_memory().unwrap();
        // Fresh vault: no records, HEAD defaults to main.
        assert!(s.branches().unwrap().is_empty());
        assert_eq!(s.head().unwrap(), crate::log::MAIN_BRANCH_ID);
        assert_eq!(s.branch(crate::log::MAIN_BRANCH_ID).unwrap().unwrap().name, "main");
        assert!(s.branch("ghost").unwrap().is_none());

        let mut b = Branch {
            branch_id: "b1".into(),
            name: "feature".into(),
            parent: Some(crate::log::MAIN_BRANCH_ID.into()),
            fork_vv: [("aa".to_string(), 3i64)].into_iter().collect(),
            created_lamport: 7,
            created_ts: 11,
            deleted: false,
        };
        s.put_branch(&b).unwrap();
        let got = s.branch("b1").unwrap().unwrap();
        assert_eq!(got, b, "branch record round-trips incl. fork_vv JSON");
        assert_eq!(s.branches().unwrap().len(), 1);

        // Upsert (rename + soft-delete) is reflected.
        b.name = "renamed".into();
        b.deleted = true;
        s.put_branch(&b).unwrap();
        assert_eq!(s.branch("b1").unwrap().unwrap().name, "renamed");
        assert!(s.branch("b1").unwrap().unwrap().deleted);

        // HEAD set/get.
        s.set_head("b1").unwrap();
        assert_eq!(s.head().unwrap(), "b1");
        s.set_head(crate::log::MAIN_BRANCH_ID).unwrap();
        assert_eq!(s.head().unwrap(), crate::log::MAIN_BRANCH_ID);
    }

    #[test]
    fn embeddings_api_roundtrip_and_model_versioned() {
        // §Embeddings: v1 ships the *substrate* (table + API), never populated by
        // the engine. The storage shape is content-addressed and model-versioned
        // so a later embedder re-embeds without touching the log.
        let s = SqliteStore::open_memory().unwrap();
        let ch = crate::oid::content_hash(b"some note body");
        assert!(s.get_embedding(&ch, "m1").unwrap().is_none());
        s.put_embedding(&ch, "m1", &[1, 2, 3, 4]).unwrap();
        s.put_embedding(&ch, "m2", &[9, 9]).unwrap(); // re-embed under a new model
        assert_eq!(s.get_embedding(&ch, "m1").unwrap().as_deref(), Some(&[1, 2, 3, 4][..]));
        assert_eq!(s.get_embedding(&ch, "m2").unwrap().as_deref(), Some(&[9, 9][..]));
        // Re-embedding the same (content, model) overwrites, log untouched.
        s.put_embedding(&ch, "m1", &[5, 6]).unwrap();
        assert_eq!(s.get_embedding(&ch, "m1").unwrap().as_deref(), Some(&[5, 6][..]));
        assert_eq!(s.row_count().unwrap(), 0, "embeddings never write the log");
    }

    // Regression: opening a vault created *before* branching must not fail. The
    // pre-branching `log` table has no `branch_id` column; the `log_branch` index
    // must therefore be created only after `migrate_branching` ALTERs the column in,
    // not eagerly in SCHEMA (which would fail the whole batch with "no such column:
    // branch_id" and abort the open — the bug behind "create vault failed").
    #[test]
    fn opens_pre_branching_db_and_migrates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("asp.db");

        // Author a pre-branching DB: just the old `log` table (no branch_id /
        // merge_parent, no log_branch index) with one row, exactly as an older
        // build would have left it on disk.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE log(
                   id TEXT PRIMARY KEY, site_id TEXT NOT NULL, lamport INTEGER NOT NULL, seq INTEGER NOT NULL,
                   ts INTEGER NOT NULL, file_id TEXT NOT NULL, kind TEXT NOT NULL, merge_class TEXT NOT NULL,
                   parent TEXT, base_hash TEXT, result_hash TEXT, path TEXT, sig BLOB,
                   UNIQUE(site_id, seq)
                 );
                 INSERT INTO log(id, site_id, lamport, seq, ts, file_id, kind, merge_class)
                 VALUES('r1','aa',1,0,0,'f1','create','text');",
            )
            .unwrap();
        }

        // Opening must succeed (previously errored before the migration could run).
        let s = SqliteStore::open(&path).unwrap();

        // The column was added and the index now exists.
        let have: std::collections::HashSet<String> = {
            let mut stmt = s.conn.prepare("PRAGMA table_info(log)").unwrap();
            stmt.query_map([], |r| r.get::<_, String>(1)).unwrap().collect::<Result<_, _>>().unwrap()
        };
        assert!(have.contains("branch_id") && have.contains("merge_parent"));
        let has_index: bool = s
            .conn
            .query_row("SELECT 1 FROM sqlite_master WHERE type='index' AND name='log_branch'", [], |_| Ok(()))
            .optional()
            .unwrap()
            .is_some();
        assert!(has_index, "log_branch index created after the migration");

        // The pre-existing row reads back on the implicit `main` branch, and a
        // re-open is idempotent (no error from re-running the migration/index).
        assert_eq!(s.head().unwrap(), crate::log::MAIN_BRANCH_ID);
        let branch: String = s.conn.query_row("SELECT branch_id FROM log WHERE id='r1'", [], |r| r.get(0)).unwrap();
        assert_eq!(branch, "main");
        drop(s);
        SqliteStore::open(&path).unwrap(); // idempotent second open
    }

    #[test]
    fn bulk_load_rebuilds_the_exact_index_set() {
        // The clone bulk load drops the log's secondary indexes and rebuilds them
        // from LOG_SECONDARY_INDEXES. Pin that the rebuilt set is byte-identical to
        // a freshly-opened store's — the strongest guarantee the rebuild definitions
        // never drift from SCHEMA + migrate_branching (a mismatch would silently
        // change query plans or the index semantics).
        let index_sql = |s: &SqliteStore| -> Vec<(String, Option<String>)> {
            let mut stmt = s
                .conn
                .prepare("SELECT name, sql FROM sqlite_master WHERE type='index' AND tbl_name='log' ORDER BY name")
                .unwrap();
            stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        };
        let s = SqliteStore::open_memory().unwrap();
        let before = index_sql(&s);
        // Sanity: all five named secondary indexes are present to begin with.
        for name in LOG_SECONDARY_INDEX_NAMES {
            assert!(before.iter().any(|(n, _)| n == name), "expected index {name} on a fresh store");
        }
        // A no-op bulk load must leave the index set exactly as it found it.
        s.bulk_load(|| Ok(())).unwrap();
        assert_eq!(before, index_sql(&s), "bulk_load must rebuild the identical index set");
        // synchronous restored to NORMAL (1), not left OFF (0).
        let sync: i64 = s.conn.query_row("PRAGMA synchronous", [], |r| r.get(0)).unwrap();
        assert_eq!(sync, 1, "synchronous must be restored to NORMAL after bulk_load");
    }

    #[test]
    fn bulk_load_rebuilds_indexes_even_on_error() {
        let s = SqliteStore::open_memory().unwrap();
        let r: AspResult<()> = s.bulk_load(|| Err(crate::error::AspError::Protocol("boom".into())));
        assert!(r.is_err(), "f's error propagates");
        // Indexes must still be back despite the error.
        let has: bool = s
            .conn
            .query_row("SELECT 1 FROM sqlite_master WHERE type='index' AND name='log_kind_branch'", [], |_| Ok(()))
            .optional()
            .unwrap()
            .is_some();
        assert!(has, "log_kind_branch rebuilt even after f errored");
    }

    #[test]
    fn branch_rows_query_uses_the_partial_index_not_a_full_scan() {
        // reconcile_branches runs branch_rows() on every branch authoring and remote
        // integration; without the partial index it is an O(log) full table scan.
        // Assert the planner uses the index so a large content log can't make every
        // reconcile O(N).
        let s = SqliteStore::open_memory().unwrap();
        let plan: String = s
            .conn
            .query_row("EXPLAIN QUERY PLAN SELECT * FROM log WHERE kind='branch'", [], |r| r.get::<_, String>(3))
            .unwrap();
        assert!(
            plan.contains("log_kind_branch"),
            "branch_rows must hit the partial index, got plan: {plan}"
        );
        assert!(!plan.contains("SCAN log"), "branch_rows must not full-scan the log, got: {plan}");
    }
}
