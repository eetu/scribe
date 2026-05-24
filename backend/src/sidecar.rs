//! `.scribe.json` sidecar files — the post-download record-keeping that
//! survives a DB wipe.
//!
//! Convention: written next to the AAXC in `SCRIBE_ORIGINAL_DIR` with
//! the same basename + `.scribe.json` suffix:
//!
//!     original/Author/Title-{asin}.aaxc
//!     original/Author/Title-{asin}.aaxc.scribe.json
//!
//! Schema lives in `scribe_shared::Sidecar`. Write at the end of each
//! successful pipeline run; read during the reconcile scan on boot or
//! manual `/api/library/reconcile` trigger.

use std::path::Path;

use scribe_shared::Sidecar;
use tokio::io::AsyncWriteExt;

use crate::error::AppError;

pub fn sidecar_path_for(aaxc_path: &Path) -> std::path::PathBuf {
    let mut p = aaxc_path.as_os_str().to_owned();
    p.push(".scribe.json");
    std::path::PathBuf::from(p)
}

pub async fn write(aaxc_path: &Path, sidecar: &Sidecar) -> Result<(), AppError> {
    let path = sidecar_path_for(aaxc_path);
    let bytes = serde_json::to_vec_pretty(sidecar).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    }
    let mut file = tokio::fs::File::create(&path)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    file.write_all(&bytes)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    file.flush()
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    tracing::debug!(path = %path.display(), "sidecar written");
    Ok(())
}

pub async fn read(path: &Path) -> Result<Sidecar, AppError> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
    serde_json::from_slice::<Sidecar>(&bytes).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))
}
