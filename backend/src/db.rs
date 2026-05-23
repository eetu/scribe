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
        conn.execute_batch(SCHEMA_V1)?;
        Ok(())
    }
}

const SCHEMA_V1: &str = r#"
CREATE TABLE IF NOT EXISTS accounts (
  id TEXT PRIMARY KEY,
  locale TEXT NOT NULL,
  email_masked TEXT NOT NULL,
  customer_name TEXT,
  last_synced_at INTEGER,
  user_sub TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_accounts_user ON accounts(user_sub);

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
"#;
