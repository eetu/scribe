use std::env;
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct Config {
    pub bind: String,
    /// When `None`, the bearer guard is bypassed — useful for local dev
    /// without juggling a token. Production deploys must set `PRESS_TOKEN`.
    pub token: Option<String>,
    pub tmp_dir: PathBuf,
    pub max_jobs: usize,
    pub ffmpeg_bin: String,
    /// Allow `file://` content_urls. Off by default: a `file://` job copies
    /// an arbitrary local path into the job dir and exposes it via
    /// `GET /jobs/{id}/aaxc`, i.e. an arbitrary-local-file-read primitive for
    /// any holder of the bearer token. The normal pipeline never sends
    /// `file://` (reconvert uses an `http://…/internal/aaxc/<token>` URL), so
    /// this stays off in prod; flip it on only for local OpenAudible-import
    /// testing.
    pub allow_file_url: bool,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let token = env::var("PRESS_TOKEN").ok().filter(|s| !s.is_empty());
        if token.is_none() {
            tracing::warn!(
                "PRESS_TOKEN unset — bearer auth disabled. Do not run like this on a reachable host."
            );
        }
        let tmp_dir = env::var("PRESS_TMP_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("scribe-press"));
        std::fs::create_dir_all(&tmp_dir)?;
        Ok(Self {
            bind: env::var("PRESS_BIND").unwrap_or_else(|_| "127.0.0.1:3005".into()),
            token,
            tmp_dir,
            max_jobs: env::var("PRESS_MAX_JOBS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(2),
            ffmpeg_bin: env::var("FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".into()),
            allow_file_url: env::var("PRESS_ALLOW_FILE_URL")
                .map(|s| s == "1" || s.eq_ignore_ascii_case("true"))
                .unwrap_or(false),
        })
    }
}
