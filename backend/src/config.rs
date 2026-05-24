use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: String,
    pub db_path: PathBuf,
    pub session_key_hex: String,
    pub dev_auth: bool,
    pub shim_url: String,
    pub press_url: Option<String>,
    pub press_token: Option<String>,
    pub library_dir: PathBuf,
    pub original_dir: PathBuf,
    pub poll_interval_min: u64,
    pub poll_jitter_percent: u32,
    pub poll_active_hour_start: u32,
    pub poll_active_hour_end: u32,
    pub job_concurrency: usize,
    pub job_retry_max: u32,
    pub job_interjob_delay_s: u64,
    pub job_interjob_jitter_percent: u32,
    pub auto_enqueue_new: bool,
    pub naming: crate::filenaming::Templates,
    pub open_registration: bool,
    pub admin_email: Option<String>,
    pub abs_url: Option<String>,
    pub abs_token: Option<String>,
    pub abs_library_id: Option<String>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            bind: env::var("SCRIBE_BIND").unwrap_or_else(|_| "0.0.0.0:3003".into()),
            db_path: env::var("SCRIBE_DB_PATH")
                .unwrap_or_else(|_| "scribe.db".into())
                .into(),
            session_key_hex: env::var("SESSION_KEY")
                .unwrap_or_else(|_| ephemeral_session_key()),
            dev_auth: env::var("DEV_AUTH").as_deref() == Ok("1"),
            shim_url: env::var("SCRIBE_SHIM_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:3004".into()),
            press_url: env::var("SCRIBE_PRESS_URL").ok(),
            press_token: env::var("SCRIBE_PRESS_TOKEN").ok(),
            library_dir: env::var("SCRIBE_LIBRARY_DIR")
                .unwrap_or_else(|_| "/mnt/audiobooks/library".into())
                .into(),
            original_dir: env::var("SCRIBE_ORIGINAL_DIR")
                .unwrap_or_else(|_| "/mnt/audiobooks/original".into())
                .into(),
            poll_interval_min: env::var("SCRIBE_POLL_INTERVAL_MIN")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(60),
            poll_jitter_percent: env::var("SCRIBE_POLL_JITTER_PERCENT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(50),
            poll_active_hour_start: env::var("SCRIBE_POLL_ACTIVE_HOUR_START")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(7),
            poll_active_hour_end: env::var("SCRIBE_POLL_ACTIVE_HOUR_END")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(23),
            job_concurrency: env::var("SCRIBE_JOB_CONCURRENCY")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(1),
            job_retry_max: env::var("SCRIBE_JOB_RETRY_MAX")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(3),
            job_interjob_delay_s: env::var("SCRIBE_JOB_INTERJOB_DELAY_S")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(60),
            job_interjob_jitter_percent: env::var("SCRIBE_JOB_INTERJOB_JITTER_PERCENT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(50),
            auto_enqueue_new: env::var("SCRIBE_AUTO_ENQUEUE")
                .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
            naming: crate::filenaming::Templates::from_env(),
            open_registration: env::var("SCRIBE_OPEN_REGISTRATION")
                .map(|s| s != "0")
                .unwrap_or(true),
            admin_email: env::var("SCRIBE_ADMIN_EMAIL").ok(),
            abs_url: env::var("ABS_URL").ok(),
            abs_token: env::var("ABS_TOKEN").ok(),
            abs_library_id: env::var("ABS_LIBRARY_ID").ok(),
        })
    }
}

fn ephemeral_session_key() -> String {
    // 128 hex chars = 64 bytes — meets axum-extra's signed cookie key length.
    // Not random across boots; sessions invalidate on restart in this fallback,
    // which is the right behaviour when nothing was configured.
    let bytes: [u8; 64] = std::array::from_fn(|i| (i as u8).wrapping_mul(31));
    let mut s = String::with_capacity(128);
    for b in &bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}
