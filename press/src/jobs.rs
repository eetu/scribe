use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use scribe_shared::{Chapter, JobEvent};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex, Semaphore};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "drm", rename_all = "snake_case")]
pub enum Drm {
    /// AAXC — per-book AES-128 key + IV (32 hex chars each, 16 bytes each).
    Aaxc { key_hex: String, iv_hex: String },
    /// AAX legacy — 4-byte (8 hex chars) account-wide secret. ffmpeg derives the
    /// per-file key + IV from this plus the file header.
    Aax { activation_bytes: String },
}

impl Drm {
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Drm::Aaxc { key_hex, iv_hex } => {
                if key_hex.len() != 32 || iv_hex.len() != 32 {
                    return Err("aaxc: key_hex and iv_hex must be 32 hex chars (16 bytes) each");
                }
                Ok(())
            }
            Drm::Aax { activation_bytes } => {
                if activation_bytes.len() != 8 {
                    return Err("aax: activation_bytes must be 8 hex chars (4 bytes)");
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobReq {
    /// HTTPS Audible CDN URL OR `file://` path for local-file test runs.
    pub content_url: String,
    pub drm: Drm,
    pub asin: String,
    pub title: String,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub narrators: Vec<String>,
    #[serde(default)]
    pub series_title: Option<String>,
    #[serde(default)]
    pub series_sequence: Option<String>,
    #[serde(default)]
    pub chapters: Vec<Chapter>,
    #[serde(default)]
    pub cover_url: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Queued,
    Downloading,
    Converting,
    Ready,
    Failed,
}

#[derive(Debug, Clone, Serialize)]
pub struct JobStatus {
    pub id: Uuid,
    pub phase: Phase,
    pub aaxc_bytes: u64,
    pub m4b_bytes: u64,
    pub error: Option<String>,
}

pub struct JobState {
    pub id: Uuid,
    pub req: JobReq,
    pub dir: PathBuf,
    pub phase: Phase,
    pub aaxc_bytes: u64,
    pub m4b_bytes: u64,
    pub error: Option<String>,
    pub events: broadcast::Sender<JobEvent>,
}

impl JobState {
    pub fn aaxc_path(&self) -> PathBuf {
        match &self.req.drm {
            Drm::Aax { .. } => self.dir.join("raw.aax"),
            Drm::Aaxc { .. } => self.dir.join("raw.aaxc"),
        }
    }
    pub fn m4b_path(&self) -> PathBuf {
        self.dir.join("out.m4b")
    }
    pub fn status(&self) -> JobStatus {
        JobStatus {
            id: self.id,
            phase: self.phase,
            aaxc_bytes: self.aaxc_bytes,
            m4b_bytes: self.m4b_bytes,
            error: self.error.clone(),
        }
    }
}

#[derive(Clone)]
pub struct JobMap {
    inner: Arc<Mutex<HashMap<Uuid, Arc<Mutex<JobState>>>>>,
    pub sem: Arc<Semaphore>,
    pub tmp_root: PathBuf,
}

impl JobMap {
    pub fn new(tmp_root: PathBuf, max_jobs: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            sem: Arc::new(Semaphore::new(max_jobs)),
            tmp_root,
        }
    }

    pub async fn create(&self, req: JobReq) -> std::io::Result<Arc<Mutex<JobState>>> {
        let id = Uuid::new_v4();
        let dir = self.tmp_root.join(id.to_string());
        tokio::fs::create_dir_all(&dir).await?;
        let (tx, _rx) = broadcast::channel(64);
        let state = Arc::new(Mutex::new(JobState {
            id,
            req,
            dir,
            phase: Phase::Queued,
            aaxc_bytes: 0,
            m4b_bytes: 0,
            error: None,
            events: tx,
        }));
        self.inner.lock().await.insert(id, state.clone());
        Ok(state)
    }

    pub async fn get(&self, id: Uuid) -> Option<Arc<Mutex<JobState>>> {
        self.inner.lock().await.get(&id).cloned()
    }

    pub async fn remove(&self, id: Uuid) -> Option<Arc<Mutex<JobState>>> {
        self.inner.lock().await.remove(&id)
    }
}

pub async fn purge_dir(dir: &Path) {
    let _ = tokio::fs::remove_dir_all(dir).await;
}
