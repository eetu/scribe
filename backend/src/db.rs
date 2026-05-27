use std::path::Path;
use std::sync::Arc;
use tokio::sync::Mutex;

use rusqlite::Connection;

/// Single-writer SQLite connection guarded by a tokio Mutex.
///
/// Scribe's write QPS is tiny (jobs, polling diffs). One connection is
/// plenty and avoids the lifetime headaches of a real pool. Reads also
/// go through the mutex — fine for this load, swap for `r2d2` later if
/// the queue UI ever feels sluggish.
#[derive(Clone)]
pub struct Db {
    inner: Arc<Mutex<Connection>>,
}

/// Informational schema marker, stamped into `user_version`. Migrations
/// no longer *gate* on it — the whole schema is declarative + idempotent
/// (every statement is CREATE/ADD/DROP IF (NOT) EXISTS), so it's applied
/// on every boot. That makes drift impossible: a watch-runner like bacon
/// can restart mid-edit and advance the version before the matching DDL
/// is written, but the next boot still converges the schema to match the
/// code regardless of what the version says.
const SCHEMA_VERSION: i64 = 4;

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Self::migrate(&conn)?;
        Ok(Self {
            inner: Arc::new(Mutex::new(conn)),
        })
    }

    pub async fn with<R>(&self, f: impl FnOnce(&Connection) -> rusqlite::Result<R>) -> rusqlite::Result<R> {
        let guard = self.inner.lock().await;
        f(&guard)
    }

    fn migrate(conn: &Connection) -> anyhow::Result<()> {
        // Declarative + idempotent — safe to run every boot, converges the
        // schema to match the code regardless of `user_version`.

        // Tables + indexes (all CREATE ... IF NOT EXISTS).
        conn.execute_batch(SCHEMA)?;

        // Columns added to existing tables after the original baseline.
        // CREATE TABLE IF NOT EXISTS won't alter a table that already
        // exists, so these add-if-missing for upgraded DBs.
        for (col, decl) in [
            ("codec", "TEXT"),
            ("bitrate_kbps", "INTEGER"),
            ("sample_rate", "INTEGER"),
            ("channels", "INTEGER"),
            ("chapters_json", "TEXT"),
        ] {
            add_column_if_missing(conn, "books", col, decl)?;
        }

        // Legacy columns from the pre-kanidm schema (admin/user split) —
        // dropped now that kanidm is the bouncer. Existence-checked so
        // this is a no-op on fresh installs and after the first drop.
        for col in ["role", "display_name"] {
            drop_column_if_exists(conn, "profile", col)?;
        }

        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    }
}

/// Idempotent `ALTER TABLE ADD COLUMN` — checks `table_info` first, so it
/// no-ops when the column already exists. Table/column names are
/// hardcoded literals (never user input), so the inline format is safe.
fn add_column_if_missing(
    conn: &Connection,
    table: &str,
    column: &str,
    decl: &str,
) -> anyhow::Result<()> {
    if !column_exists(conn, table, column)? {
        conn.execute(&format!("ALTER TABLE {table} ADD COLUMN {column} {decl}"), [])?;
    }
    Ok(())
}

/// Idempotent `ALTER TABLE DROP COLUMN` — no-ops when the column is
/// already gone. Hardcoded identifiers, so the inline format is safe.
fn drop_column_if_exists(conn: &Connection, table: &str, column: &str) -> anyhow::Result<()> {
    if column_exists(conn, table, column)? {
        conn.execute(&format!("ALTER TABLE {table} DROP COLUMN {column}"), [])?;
    }
    Ok(())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> anyhow::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let found = stmt
        .query_map([], |r| r.get::<_, String>(1))?
        .filter_map(Result::ok)
        .any(|name| name == column);
    Ok(found)
}

// Full declarative schema. Every statement is idempotent so the whole
// batch runs on every boot (see `migrate`). `removed_books` is the
// tombstone for user-removed books: the row is gone but a leftover
// `*.scribe.json` sidecar (kept on disk for its durable voucher) would
// otherwise let the boot reconcile pass resurrect the job — reconcile
// skips tombstoned asins, while library sync clears the tombstone if
// Audible lists the title again (a genuine re-purchase).
const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS profile (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_sub TEXT UNIQUE,
  email TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_profile_sub ON profile(user_sub);

CREATE TABLE IF NOT EXISTS profile_settings (
  profile_id INTEGER NOT NULL REFERENCES profile(id) ON DELETE CASCADE,
  key TEXT NOT NULL,
  value TEXT NOT NULL,
  PRIMARY KEY (profile_id, key)
);

CREATE TABLE IF NOT EXISTS accounts (
  id TEXT PRIMARY KEY,
  profile_id INTEGER NOT NULL REFERENCES profile(id) ON DELETE CASCADE,
  locale TEXT NOT NULL,
  email_masked TEXT NOT NULL,
  customer_name TEXT,
  last_synced_at INTEGER
);
CREATE INDEX IF NOT EXISTS idx_accounts_profile ON accounts(profile_id);

CREATE TABLE IF NOT EXISTS books (
  asin TEXT NOT NULL,
  account_id TEXT NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  title TEXT NOT NULL,
  subtitle TEXT,
  authors_json TEXT NOT NULL,
  narrators_json TEXT NOT NULL,
  series_title TEXT,
  series_sequence TEXT,
  runtime_length_ms INTEGER,
  cover_url TEXT,
  status TEXT NOT NULL,
  purchase_date TEXT,
  first_seen_at INTEGER NOT NULL,
  codec TEXT,
  bitrate_kbps INTEGER,
  sample_rate INTEGER,
  channels INTEGER,
  chapters_json TEXT,
  PRIMARY KEY (asin, account_id)
);
CREATE INDEX IF NOT EXISTS idx_books_account ON books(account_id);
CREATE INDEX IF NOT EXISTS idx_books_purchase ON books(purchase_date);

CREATE TABLE IF NOT EXISTS jobs (
  id TEXT PRIMARY KEY,
  asin TEXT NOT NULL,
  account_id TEXT NOT NULL,
  status TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  error TEXT,
  m4b_path TEXT,
  aaxc_path TEXT
);
CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status);
CREATE INDEX IF NOT EXISTS idx_jobs_account ON jobs(account_id);

CREATE TABLE IF NOT EXISTS removed_books (
  asin TEXT NOT NULL,
  account_id TEXT NOT NULL,
  removed_at INTEGER NOT NULL,
  PRIMARY KEY (asin, account_id)
);
"#;

