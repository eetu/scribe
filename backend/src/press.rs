//! HTTP client for scribe-press (mini worker) + NAS streaming sink + ABS notify.
//!
//! The Pi never holds either the AAXC or the M4B fully in RAM: each artifact
//! is fetched as a streaming response and piped straight into the NAS file via
//! `tokio::io::copy`. Press holds the bytes briefly on its SSD until the Pi
//! pulls them, then we DELETE the job.

use std::path::Path;

use futures_util::TryStreamExt;
use reqwest::header::AUTHORIZATION;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio_util::io::StreamReader;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "drm", rename_all = "snake_case")]
pub enum PressDrm {
    Aaxc { key_hex: String, iv_hex: String },
    Aax { activation_bytes: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct PressJobReq {
    pub content_url: String,
    pub drm: PressDrm,
    pub asin: String,
    pub title: String,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub authors: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub narrators: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series_sequence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PressJobCreated {
    pub job_id: Uuid,
}

#[derive(Debug, Deserialize, Clone)]
pub struct PressJobStatus {
    pub id: Uuid,
    pub phase: String,
    pub aaxc_bytes: u64,
    pub m4b_bytes: u64,
    pub error: Option<String>,
}

pub struct PressClient<'a> {
    state: &'a AppState,
}

impl<'a> PressClient<'a> {
    pub fn new(state: &'a AppState) -> Self {
        Self { state }
    }

    /// Press is reachable when a URL is configured. Token is optional —
    /// matches press-side behaviour where empty `PRESS_TOKEN` disables
    /// bearer auth for local dev.
    pub fn is_configured(&self) -> bool {
        self.state.cfg.press_url.is_some()
    }

    fn base(&self) -> Result<&str, AppError> {
        self.state
            .cfg
            .press_url
            .as_deref()
            .ok_or_else(|| AppError::BadRequest("SCRIBE_PRESS_URL not configured".into()))
    }

    /// Attach `Authorization: Bearer ...` only when a token is configured.
    /// Press accepts unauthenticated calls when its own `PRESS_TOKEN` is
    /// also unset, so this stays in sync with the worker's policy.
    fn auth(&self, b: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match self.state.cfg.press_token.as_deref() {
            Some(t) if !t.is_empty() => b.header(AUTHORIZATION, format!("Bearer {t}")),
            _ => b,
        }
    }

    pub async fn health(&self) -> Result<bool, AppError> {
        let Some(url) = &self.state.cfg.press_url else {
            return Ok(false);
        };
        let r = self
            .state
            .http
            .get(format!("{}/health", url.trim_end_matches('/')))
            .send()
            .await;
        Ok(matches!(r, Ok(resp) if resp.status().is_success()))
    }

    pub async fn submit(&self, req: &PressJobReq) -> Result<Uuid, AppError> {
        let url = format!("{}/jobs", self.base()?.trim_end_matches('/'));
        let r = self
            .auth(self.state.http.post(url))
            .json(req)
            .send()
            .await?
            .error_for_status()?
            .json::<PressJobCreated>()
            .await?;
        Ok(r.job_id)
    }

    pub async fn status(&self, job_id: Uuid) -> Result<PressJobStatus, AppError> {
        let url = format!("{}/jobs/{}", self.base()?.trim_end_matches('/'), job_id);
        Ok(self
            .auth(self.state.http.get(url))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub async fn stream_to_file(&self, job_id: Uuid, artifact: Artifact, dest: &Path) -> Result<u64, AppError> {
        let url = format!(
            "{}/jobs/{}/{}",
            self.base()?.trim_end_matches('/'),
            job_id,
            artifact.path_segment()
        );
        let resp = self
            .auth(self.state.http.get(url))
            .send()
            .await?
            .error_for_status()?;

        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
        }

        let mut file = tokio::fs::File::create(dest)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
        let stream = resp
            .bytes_stream()
            .map_err(std::io::Error::other);
        let mut reader = StreamReader::new(stream);
        let bytes = tokio::io::copy(&mut reader, &mut file)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
        file.flush()
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
        Ok(bytes)
    }

    pub async fn delete(&self, job_id: Uuid) -> Result<(), AppError> {
        let url = format!("{}/jobs/{}", self.base()?.trim_end_matches('/'), job_id);
        self.auth(self.state.http.delete(url))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Artifact {
    /// Original encrypted file. Written to `SCRIBE_ORIGINAL_DIR`.
    Aaxc,
    /// Decrypted lossless M4B. Written to `SCRIBE_LIBRARY_DIR`.
    M4b,
}

impl Artifact {
    fn path_segment(self) -> &'static str {
        match self {
            Artifact::Aaxc => "aaxc",
            Artifact::M4b => "m4b",
        }
    }
}

/// Notify audiobookshelf to rescan its library after a successful write.
///
/// Non-fatal: a failure here doesn't fail the job. The book is on the share;
/// ABS's own watcher will pick it up on its next sweep, this just makes it
/// instant.
pub async fn notify_abs(state: &AppState) -> Result<(), AppError> {
    let (Some(url), Some(token), Some(lib)) = (
        state.cfg.abs_url.as_deref(),
        state.cfg.abs_token.as_deref(),
        state.cfg.abs_library_id.as_deref(),
    ) else {
        tracing::debug!("ABS_* env vars unset, skipping rescan notify");
        return Ok(());
    };
    let endpoint = format!("{}/api/libraries/{}/scan", url.trim_end_matches('/'), lib);
    let resp = state
        .http
        .post(&endpoint)
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => {
            tracing::info!("ABS rescan triggered");
            Ok(())
        }
        Ok(r) => {
            tracing::warn!(status = ?r.status(), "ABS rescan returned non-success");
            Ok(())
        }
        Err(e) => {
            tracing::warn!(error = %e, "ABS rescan request failed");
            Ok(())
        }
    }
}
