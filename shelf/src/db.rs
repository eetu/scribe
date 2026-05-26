//! Read-only SQLite handle for scribe's database.
//!
//! Opens the file with `SQLITE_OPEN_READ_ONLY` so a buggy query path
//! can't accidentally mutate scribe state. We deliberately do not run
//! migrations or take a writer lock — scribe owns the schema and is
//! the only process that touches it for writes.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{Connection, OpenFlags};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct Db {
    path: PathBuf,
    /// Single shared connection. ABS endpoints are read-heavy but
    /// low-QPS (a handful of clients browsing). Per-request open is
    /// cheaper than a pool here. Lock keeps `rusqlite`'s non-Sync
    /// connection safely shared.
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        // WAL mode is set by the writer; readers just consume it. Set
        // a short busy timeout so writer commits don't make us 5xx.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Ok(Self {
            path: path.to_path_buf(),
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn with<F, T>(&self, f: F) -> anyhow::Result<T>
    where
        F: FnOnce(&Connection) -> anyhow::Result<T> + Send + 'static,
        T: Send + 'static,
    {
        let guard = self.conn.lock().await;
        f(&guard)
    }
}
