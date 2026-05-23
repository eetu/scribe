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
