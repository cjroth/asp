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
CREATE INDEX IF NOT EXISTS log_branch ON log(branch_id);
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
"#;

pub struct SqliteStore {
    conn: Connection,
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

    /// Count of live (non-deleted) materialized files — a single aggregate, so
    /// the status poll never materializes every `FileRow` just to count them.
    pub fn live_file_count(&self) -> AspResult<u64> {
        Ok(self.conn.query_row("SELECT COUNT(*) FROM files WHERE deleted=0", [], |r| r.get::<_, i64>(0))? as u64)
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
}
