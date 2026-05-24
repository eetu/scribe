use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BookMeta {
    pub asin: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub authors: Vec<String>,
    pub narrators: Vec<String>,
    pub series_title: Option<String>,
    pub series_sequence: Option<String>,
    pub runtime_length_ms: u64,
    pub release_date: Option<String>,
    pub publisher: Option<String>,
    pub language: Option<String>,
    pub cover_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chapter {
    pub title: String,
    pub start_offset_ms: u64,
    pub length_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSpec {
    pub job_id: Uuid,
    pub content_url: String,
    pub key_hex: String,
    pub iv_hex: String,
    pub codec: String,
    pub chapters: Vec<Chapter>,
    pub cover_url: Option<String>,
    pub meta: BookMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobEvent {
    Queued,
    Downloading { bytes_done: u64, bytes_total: Option<u64> },
    Converting { seconds_done: u64, seconds_total: Option<u64> },
    Ready,
    Failed { message: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Queued,
    FetchingVoucher,
    Downloading,
    Converting,
    WritingNas,
    Done,
    Failed,
    Cancelled,
}

/// Persisted next to each completed download (in `original_dir`).
///
/// Source of truth that survives a DB wipe — on boot, scribe walks
/// original_dir for `*.scribe.json` files and re-creates the matching
/// `jobs` rows with status=done. Pair this with the `asin=` ffmpeg
/// metadata baked into the M4B so even a moved/renamed file can be
/// traced back to its purchase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sidecar {
    pub asin: String,
    pub account_id: String,
    pub title: String,
    pub downloaded_at: i64,
    pub m4b_path: String,
    pub aaxc_path: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub voucher_refresh_date: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub customer_name: Option<String>,
    #[serde(default = "default_version")]
    pub scribe_version: String,
}

fn default_version() -> String {
    "unknown".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: Uuid,
    pub asin: String,
    pub account_id: String,
    pub status: JobStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub error: Option<String>,
}
