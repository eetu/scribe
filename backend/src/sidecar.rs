//! `.scribe.json` sidecar files — the post-download record-keeping that
//! survives a DB wipe.
//!
//! Convention: written next to the AAXC in `SCRIBE_ORIGINAL_DIR` with
//! the same basename + `.scribe.json` suffix:
//!
//! ```text
//! original/Author/Title-{asin}.aaxc
//! original/Author/Title-{asin}.aaxc.scribe.json
//! ```
//!
//! Schema lives in `scribe_shared::Sidecar`. Write at the end of each
//! successful pipeline run; read during the reconcile scan on boot or
//! manual `/api/library/reconcile` trigger.

use std::path::{Path, PathBuf};

use scribe_shared::Sidecar;
use tokio::io::AsyncWriteExt;

use crate::error::AppError;

pub fn sidecar_path_for(aaxc_path: &Path) -> PathBuf {
    let mut p = aaxc_path.as_os_str().to_owned();
    p.push(".scribe.json");
    PathBuf::from(p)
}

/// Inverse of [`sidecar_path_for`]: the AAXC path a sidecar at
/// `sidecar_path` would have been written next to. `None` when the path
/// doesn't carry the `.scribe.json` suffix `sidecar_path_for` appends, or
/// isn't valid UTF-8.
pub fn aaxc_path_for(sidecar_path: &Path) -> Option<PathBuf> {
    sidecar_path
        .to_str()?
        .strip_suffix(".scribe.json")
        .map(PathBuf::from)
}

pub async fn write(aaxc_path: &Path, sidecar: &Sidecar) -> Result<(), AppError> {
    let path = sidecar_path_for(aaxc_path);
    let bytes =
        serde_json::to_vec_pretty(sidecar).map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aaxc_path_for_inverts_sidecar_path_for() {
        let aaxc = Path::new("/mnt/audiobooks/original/A/T-ASIN.aaxc");
        let sc_path = sidecar_path_for(aaxc);
        assert_eq!(aaxc_path_for(&sc_path).as_deref(), Some(aaxc));
    }

    #[test]
    fn aaxc_path_for_rejects_unrelated_suffix() {
        assert_eq!(
            aaxc_path_for(Path::new("/mnt/audiobooks/original/A/T.aaxc")),
            None
        );
    }
}
