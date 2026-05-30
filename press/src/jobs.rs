use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
                // Charset check, not just length: these flow straight onto the
                // ffmpeg command line (-audible_key/-audible_iv). A non-hex value
                // (e.g. one starting with `-`) would be parsed by ffmpeg as a
                // flag rather than a key. Reject anything that isn't hex.
                if !key_hex.bytes().all(|b| b.is_ascii_hexdigit())
                    || !iv_hex.bytes().all(|b| b.is_ascii_hexdigit())
                {
                    return Err("aaxc: key_hex and iv_hex must be hexadecimal");
                }
                Ok(())
            }
            Drm::Aax { activation_bytes } => {
                if activation_bytes.len() != 8 {
                    return Err("aax: activation_bytes must be 8 hex chars (4 bytes)");
                }
                if !activation_bytes.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err("aax: activation_bytes must be hexadecimal");
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Drm;

    fn aaxc(key: &str, iv: &str) -> Drm {
        Drm::Aaxc { key_hex: key.into(), iv_hex: iv.into() }
    }

    #[test]
    fn aaxc_valid_hex_ok() {
        assert!(aaxc(&"a".repeat(32), &"0".repeat(32)).validate().is_ok());
    }

    #[test]
    fn aaxc_wrong_length_rejected() {
        assert!(aaxc(&"a".repeat(31), &"0".repeat(32)).validate().is_err());
    }

    #[test]
    fn aaxc_non_hex_rejected() {
        // right length, but not hex — would otherwise reach the ffmpeg argv.
        assert!(aaxc("-y -f mp4 /etc/passwd zzzzzzzzzz", &"0".repeat(32))
            .validate()
            .is_err());
        assert!(aaxc(&"g".repeat(32), &"0".repeat(32)).validate().is_err());
    }

    #[test]
    fn aax_charset_enforced() {
        assert!(Drm::Aax { activation_bytes: "deadbeef".into() }.validate().is_ok());
        assert!(Drm::Aax { activation_bytes: "-y deadb".into() }.validate().is_err());
        assert!(Drm::Aax { activation_bytes: "zzzzzzzz".into() }.validate().is_err());
    }

    use super::{JobMap, JobReq};
    use uuid::Uuid;

    fn sample_req() -> JobReq {
        JobReq {
            content_url: "https://example.invalid/x".into(),
            drm: aaxc(&"a".repeat(32), &"0".repeat(32)),
            asin: "B00TEST".into(),
            title: "t".into(),
            authors: vec![],
            narrators: vec![],
            series_title: None,
            series_sequence: None,
            chapters: vec![],
            cover_url: None,
        }
    }

    #[tokio::test]
    async fn sweep_removes_aged_jobs_and_purges_tmp() {
        let tmp = std::env::temp_dir().join(format!("press-sweep-test-{}", Uuid::new_v4()));
        let map = JobMap::new(tmp.clone(), 1);
        let st = map.create(sample_req()).await.unwrap();
        let (id, dir) = {
            let g = st.lock().await;
            (g.id, g.dir.clone())
        };
        assert!(map.get(id).await.is_some());
        assert!(dir.exists());

        // max_age 0 → every job counts as aged out.
        let swept = map.sweep(std::time::Duration::ZERO).await;
        assert_eq!(swept, 1);
        assert!(map.get(id).await.is_none(), "job removed from map");
        assert!(!dir.exists(), "tmp dir purged");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn sweep_keeps_fresh_jobs() {
        let tmp = std::env::temp_dir().join(format!("press-sweep-fresh-{}", Uuid::new_v4()));
        let map = JobMap::new(tmp.clone(), 1);
        let st = map.create(sample_req()).await.unwrap();
        let id = st.lock().await.id;
        // A 1h cutoff leaves a just-created job alone.
        let swept = map.sweep(std::time::Duration::from_secs(3600)).await;
        assert_eq!(swept, 0);
        assert!(map.get(id).await.is_some());
        let _ = std::fs::remove_dir_all(&tmp);
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
    /// Advertised Content-Length from the Audible CDN. `None` if the
    /// upstream didn't send one (rare); frontend renders an
    /// indeterminate progress bar in that case.
    pub aaxc_bytes_total: Option<u64>,
    pub m4b_bytes: u64,
    pub error: Option<String>,
}

pub struct JobState {
    pub id: Uuid,
    pub req: JobReq,
    pub dir: PathBuf,
    /// When the job was created — for the aged-out sweep.
    pub created: Instant,
    pub phase: Phase,
    pub aaxc_bytes: u64,
    pub aaxc_bytes_total: Option<u64>,
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
            aaxc_bytes_total: self.aaxc_bytes_total,
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
            created: Instant::now(),
            phase: Phase::Queued,
            aaxc_bytes: 0,
            aaxc_bytes_total: None,
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

    /// Drop jobs older than `max_age`: remove the entry and purge its tmp
    /// dir. Without this, a job the backend never DELETEs (crash, lost
    /// connection) leaks both its scratch files and its in-memory state —
    /// which holds the per-book voucher key/iv — until the process restarts.
    /// Returns how many were swept.
    pub async fn sweep(&self, max_age: Duration) -> usize {
        let now = Instant::now();
        // Snapshot (id, Arc) and release the map lock before locking any job,
        // so we never hold the map lock across a per-job lock.
        let entries: Vec<(Uuid, Arc<Mutex<JobState>>)> = {
            let map = self.inner.lock().await;
            map.iter().map(|(k, v)| (*k, v.clone())).collect()
        };
        let mut stale = Vec::new();
        for (id, st) in entries {
            if now.duration_since(st.lock().await.created) >= max_age {
                stale.push(id);
            }
        }
        let mut swept = 0;
        for id in stale {
            if let Some(st) = self.remove(id).await {
                let dir = st.lock().await.dir.clone();
                purge_dir(&dir).await;
                swept += 1;
            }
        }
        swept
    }
}

pub async fn purge_dir(dir: &Path) {
    let _ = tokio::fs::remove_dir_all(dir).await;
}
