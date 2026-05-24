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

/// Current schema version. Bump + add a migration block when shipping
/// a schema change to a deployed instance. Anything from `0` is a fresh
/// install and runs the full `SCHEMA` batch.
const SCHEMA_VERSION: i64 = 1;

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
        let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if current < 1 {
            conn.execute_batch(SCHEMA)?;
        }
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(())
    }
}

const SCHEMA: &str = r#"
CREATE TABLE profile (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_sub TEXT UNIQUE,
  email TEXT NOT NULL UNIQUE,
  role TEXT NOT NULL DEFAULT 'user',
  display_name TEXT,
  created_at INTEGER NOT NULL
);
CREATE INDEX idx_profile_sub ON profile(user_sub);

CREATE TABLE profile_settings (
  profile_id INTEGER NOT NULL REFERENCES profile(id) ON DELETE CASCADE,
  key TEXT NOT NULL,
  value TEXT NOT NULL,
  PRIMARY KEY (profile_id, key)
);

CREATE TABLE accounts (
  id TEXT PRIMARY KEY,
  profile_id INTEGER NOT NULL REFERENCES profile(id) ON DELETE CASCADE,
  locale TEXT NOT NULL,
  email_masked TEXT NOT NULL,
  customer_name TEXT,
  last_synced_at INTEGER
);
CREATE INDEX idx_accounts_profile ON accounts(profile_id);

CREATE TABLE books (
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
  PRIMARY KEY (asin, account_id)
);
CREATE INDEX idx_books_account ON books(account_id);
CREATE INDEX idx_books_purchase ON books(purchase_date);

CREATE TABLE jobs (
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
CREATE INDEX idx_jobs_status ON jobs(status);
CREATE INDEX idx_jobs_account ON jobs(account_id);
"#;
