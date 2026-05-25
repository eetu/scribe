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
    #[serde(default)]
    pub aaxc_bytes_total: Option<u64>,
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
        // /health is anonymous by convention. Press exempts it from its
        // own bearer guard, and any reverse proxy in front of press is
        // expected to do the same (see mini/tasks/caddy.py site_block).
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

    /// Stream an artifact from press into `dest`. When a progress sender +
    /// phase label are provided, fans out `Progress` events as bytes land
    /// so the UI doesn't go silent during the multi-minute LAN copy.
    pub async fn stream_to_file(
        &self,
        job_id: Uuid,
        artifact: Artifact,
        dest: &Path,
        progress: Option<(&tokio::sync::broadcast::Sender<crate::queue::QueueEvent>, &str)>,
    ) -> Result<u64, AppError> {
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
        let total = resp.content_length();

        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
        }

        // Write to <dest>.partial first, rename to <dest> on success. A
        // press restart, network blip, or worker crash mid-stream then
        // leaves only a `.partial` file behind — ABS never scans a
        // half-written canonical path, and a sweep can identify
        // abandoned writes by suffix alone.
        let partial = {
            let mut p = dest.as_os_str().to_owned();
            p.push(".partial");
            std::path::PathBuf::from(p)
        };
        let mut file = tokio::fs::File::create(&partial)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
        let mut stream = resp.bytes_stream();
        let mut bytes: u64 = 0;
        // Throttle Progress emission to ~once per 500ms — the SSE consumer
        // doesn't need finer than that and unbounded broadcast traffic
        // would spam the channel for fast LAN copies.
        let mut last_emit = std::time::Instant::now();
        if let Some((tx, phase)) = progress {
            let _ = tx.send(crate::queue::QueueEvent::Progress {
                phase: phase.to_string(),
                bytes_done: 0,
                bytes_total: total,
            });
        }
        while let Some(chunk) = stream
            .try_next()
            .await
            .map_err(|e| AppError::Upstream(e.to_string()))?
        {
            file.write_all(&chunk)
                .await
                .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
            bytes += chunk.len() as u64;
            if let Some((tx, phase)) = progress {
                if last_emit.elapsed() >= std::time::Duration::from_millis(500) {
                    last_emit = std::time::Instant::now();
                    let _ = tx.send(crate::queue::QueueEvent::Progress {
                        phase: phase.to_string(),
                        bytes_done: bytes,
                        bytes_total: total,
                    });
                }
            }
        }
        file.flush()
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
        drop(file);
        tokio::fs::rename(&partial, dest)
            .await
            .map_err(|e| AppError::Internal(anyhow::anyhow!(e)))?;
        if let Some((tx, phase)) = progress {
            let _ = tx.send(crate::queue::QueueEvent::Progress {
                phase: phase.to_string(),
                bytes_done: bytes,
                bytes_total: total,
            });
        }
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
