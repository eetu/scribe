use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: String,
    /// scribe's SQLite file, mounted into the shelf container read-only.
    pub db_path: PathBuf,
    /// Canonical M4B output tree (same as scribe's SCRIBE_LIBRARY_DIR).
    /// shelf streams files from here over Range-capable responses.
    pub library_dir: PathBuf,
    /// Bearer token clients must present. Treated as opaque; rotate by
    /// changing the env value (no DB row to maintain).
    pub api_key: String,
    /// Display name surfaced via /api/libraries.
    pub library_name: String,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let bind = env::var("SHELF_BIND").unwrap_or_else(|_| "0.0.0.0:3006".into());
        let db_path = env::var("SHELF_DB_PATH")
            .unwrap_or_else(|_| "/data/scribe.db".into())
            .into();
        let library_dir = env::var("SHELF_LIBRARY_DIR")
            .unwrap_or_else(|_| "/mnt/audiobooks/audible/books".into())
            .into();
        let api_key = env::var("SHELF_API_KEY")
            .map_err(|_| anyhow::anyhow!("SHELF_API_KEY required"))?;
        if api_key.is_empty() {
            anyhow::bail!("SHELF_API_KEY must not be empty");
        }
        let library_name =
            env::var("SHELF_LIBRARY_NAME").unwrap_or_else(|_| "Audiobooks".into());
        Ok(Self {
            bind,
            db_path,
            library_dir,
            api_key,
            library_name,
        })
    }
}
