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
use crate::error::AspResult;
use crate::log::{Kind, LogRow, MergeClass};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::BTreeMap;
use std::path::Path;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS blobs(content_hash TEXT PRIMARY KEY, bytes BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS log(
  id TEXT PRIMARY KEY, site_id TEXT NOT NULL, lamport INTEGER NOT NULL, seq INTEGER NOT NULL,
  ts INTEGER NOT NULL, file_id TEXT NOT NULL, kind TEXT NOT NULL, merge_class TEXT NOT NULL,
  parent TEXT, base_hash TEXT, result_hash TEXT, path TEXT, sig BLOB,
  UNIQUE(site_id, seq)
);
CREATE INDEX IF NOT EXISTS log_file ON log(file_id);
CREATE INDEX IF NOT EXISTS log_site ON log(site_id, seq);
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
"#;

pub struct Store {
    conn: Connection,
}

/// A materialized file row (§files).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRow {
    pub file_id: String,
    pub path: String,
    pub result_hash: Option<String>,
    pub merge_class: MergeClass,
    pub deleted: bool,
    pub lamport: u64,
    pub site_id: String,
    pub conflict: bool,
}

impl Store {
    pub fn open(path: &Path) -> AspResult<Store> {
        let conn = Connection::open(path)?;
        Self::init(conn)
    }

    pub fn open_memory() -> AspResult<Store> {
        let conn = Connection::open_in_memory()?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> AspResult<Store> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;",
        )?;
        conn.execute_batch(SCHEMA)?;
        Ok(Store { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    // ----- blobs -----

    pub fn put_blob(&self, bytes: &[u8]) -> AspResult<String> {
        let h = crate::oid::content_hash(bytes);
        self.conn.execute(
            "INSERT OR IGNORE INTO blobs(content_hash, bytes) VALUES (?1, ?2)",
            params![h, bytes],
        )?;
        Ok(h)
    }

    pub fn get_blob(&self, hash: &str) -> AspResult<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row("SELECT bytes FROM blobs WHERE content_hash=?1", params![hash], |r| {
                r.get::<_, Vec<u8>>(0)
            })
            .optional()?)
    }

    pub fn has_blob(&self, hash: &str) -> AspResult<bool> {
        Ok(self
            .conn
            .query_row("SELECT 1 FROM blobs WHERE content_hash=?1", params![hash], |_| Ok(()))
            .optional()?
            .is_some())
    }

    // ----- log -----

    /// Append a row idempotently (dedup by Merkle id). Returns true if newly added.
    pub fn append_row(&self, row: &LogRow) -> AspResult<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO log(id, site_id, lamport, seq, ts, file_id, kind, merge_class, parent, base_hash, result_hash, path, sig)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                row.id, row.site_id, row.lamport, row.seq, row.ts, row.file_id,
                row.kind.as_str(), row.merge_class.as_str(), row.parent, row.base_hash,
                row.result_hash, row.path, if row.sig.is_empty() { None } else { Some(&row.sig) }
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

    /// Rows authored by `site` with `seq > after`, ascending — what a peer is
    /// missing per the version vector.
    pub fn rows_after(&self, site: &str, after: i64) -> AspResult<Vec<LogRow>> {
        let mut stmt = self
            .conn
            .prepare("SELECT * FROM log WHERE site_id=?1 AND seq>?2 ORDER BY seq")?;
        let rows = stmt.query_map(params![site, after], Self::row_from)?;
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

    pub fn replace_files(&self, files: &[FileRow]) -> AspResult<()> {
        self.conn.execute("DELETE FROM files", [])?;
        for f in files {
            self.conn.execute(
                "INSERT INTO files(file_id, path, result_hash, merge_class, deleted, lamport, site_id, conflict)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    f.file_id, f.path, f.result_hash, f.merge_class.as_str(),
                    f.deleted as i64, f.lamport as i64, f.site_id, f.conflict as i64
                ],
            )?;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blob_and_log_roundtrip_with_counters() {
        let s = Store::open_memory().unwrap();
        let h = s.put_blob(b"hi").unwrap();
        assert_eq!(s.get_blob(&h).unwrap().as_deref(), Some(&b"hi"[..]));
        assert!(s.has_blob(&h).unwrap());
        // Counters derive from the durable log.
        assert_eq!(s.next_lamport(0).unwrap(), 1);
        assert_eq!(s.next_seq("aa").unwrap(), 0);
    }

    #[test]
    fn embeddings_api_roundtrip_and_model_versioned() {
        // §Embeddings: v1 ships the *substrate* (table + API), never populated by
        // the engine. The storage shape is content-addressed and model-versioned
        // so a later embedder re-embeds without touching the log.
        let s = Store::open_memory().unwrap();
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
