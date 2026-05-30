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
    /// Optional shelf (read-only ABS-compat sidecar) base URL. Used
    /// only to surface health + the matching API key in the UI so a
    /// logged-in user can copy it into Listen This or another ABS
    /// client. Scribe never talks to shelf for any other purpose.
    pub shelf_url: Option<String>,
    pub shelf_api_key: Option<String>,
    pub library_dir: PathBuf,
    pub original_dir: PathBuf,
    /// Where cached cover images live, keyed by asin. Restic-backed
    /// (sits under /var/lib/scribe → /data in-container). Covers survive
    /// Amazon pulling a title's CDN art and a books-table wipe alike.
    pub covers_dir: PathBuf,
    /// LAN URL of this scribe instance, as seen from the press worker.
    /// Used to mint short-lived `/internal/aaxc/<token>` URLs that point
    /// press at locally-stored AAXC files during a reconvert. Unset =
    /// reconvert disabled.
    pub internal_url: Option<String>,
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
    pub abs_url: Option<String>,
    pub abs_token: Option<String>,
    pub abs_library_id: Option<String>,
    pub oidc: Option<OidcSettings>,
}

#[derive(Debug, Clone)]
pub struct OidcSettings {
    pub issuer: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_url: String,
}

impl OidcSettings {
    fn from_env() -> Option<Self> {
        let issuer = env::var("OIDC_ISSUER").ok().filter(|s| !s.is_empty())?;
        let client_id = env::var("OIDC_CLIENT_ID").ok().filter(|s| !s.is_empty())?;
        let client_secret = env::var("OIDC_CLIENT_SECRET").ok().filter(|s| !s.is_empty())?;
        let redirect_url = env::var("OIDC_REDIRECT_URL").ok().filter(|s| !s.is_empty())?;
        Some(Self { issuer, client_id, client_secret, redirect_url })
    }
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let dev_auth = env::var("DEV_AUTH").as_deref() == Ok("1");
        let session_key_hex = resolve_session_key(dev_auth)?;
        Ok(Self {
            bind: env::var("SCRIBE_BIND").unwrap_or_else(|_| "0.0.0.0:3003".into()),
            db_path: env::var("SCRIBE_DB_PATH")
                .unwrap_or_else(|_| "scribe.db".into())
                .into(),
            session_key_hex,
            dev_auth,
            shim_url: env::var("SCRIBE_SHIM_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:3004".into()),
            press_url: env::var("SCRIBE_PRESS_URL").ok(),
            press_token: env::var("SCRIBE_PRESS_TOKEN").ok(),
            shelf_url: env::var("SCRIBE_SHELF_URL").ok().filter(|s| !s.is_empty()),
            shelf_api_key: env::var("SCRIBE_SHELF_API_KEY")
                .ok()
                .filter(|s| !s.is_empty()),
            library_dir: env::var("SCRIBE_LIBRARY_DIR")
                .unwrap_or_else(|_| "/mnt/audiobooks/library".into())
                .into(),
            original_dir: env::var("SCRIBE_ORIGINAL_DIR")
                .unwrap_or_else(|_| "/mnt/audiobooks/original".into())
                .into(),
            // Relative default mirrors SCRIBE_DB_PATH ("scribe.db") so local
            // dev writes under the cwd; the container sets the absolute
            // /data/covers explicitly (see raspi tasks/scribe.py).
            covers_dir: env::var("SCRIBE_COVERS_DIR")
                .unwrap_or_else(|_| "covers".into())
                .into(),
            internal_url: env::var("SCRIBE_INTERNAL_URL")
                .ok()
                .filter(|s| !s.is_empty()),
            poll_interval_min: env::var("SCRIBE_POLL_INTERVAL_MIN")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(60),
            poll_jitter_percent: env::var("SCRIBE_POLL_JITTER_PERCENT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(50),
            // Clamp to a valid clock hour (0–23). `% 24` keeps a stray value
            // sane and maps "24" → 0 (midnight), so an end of 24 reads as
            // "through midnight" rather than silently disabling the window.
            poll_active_hour_start: env::var("SCRIBE_POLL_ACTIVE_HOUR_START")
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
                .map(|h| h % 24)
                .unwrap_or(7),
            poll_active_hour_end: env::var("SCRIBE_POLL_ACTIVE_HOUR_END")
                .ok()
                .and_then(|s| s.parse::<u32>().ok())
                .map(|h| h % 24)
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
            abs_url: env::var("ABS_URL").ok(),
            abs_token: env::var("ABS_TOKEN").ok(),
            abs_library_id: env::var("ABS_LIBRARY_ID").ok(),
            oidc: OidcSettings::from_env(),
        })
    }
}

/// Resolve the signed-cookie key. The cookie is the *entire* auth
/// credential (sessions aren't persisted server-side), so the key must be
/// strong and secret:
///   - `SESSION_KEY` set → require ≥64 bytes of valid hex, else hard error.
///   - unset + `DEV_AUTH` → random per-boot key (sessions drop on restart).
///   - unset + prod → fail closed. A predictable/derivable key would let
///     anyone forge a `scribe_session` cookie and authenticate as any user.
fn resolve_session_key(dev_auth: bool) -> anyhow::Result<String> {
    match env::var("SESSION_KEY") {
        Ok(k) if !k.trim().is_empty() => {
            let k = k.trim().to_string();
            let decoded = hex::decode(&k).map_err(|_| {
                anyhow::anyhow!("SESSION_KEY must be hex (128 chars = 64 bytes)")
            })?;
            if decoded.len() < 64 {
                anyhow::bail!(
                    "SESSION_KEY too short: {} bytes decoded, need ≥64 (128 hex chars). \
                     Generate one with `openssl rand -hex 64`",
                    decoded.len()
                );
            }
            Ok(k)
        }
        _ => {
            if dev_auth {
                tracing::warn!(
                    "SESSION_KEY unset; using a random ephemeral key (DEV_AUTH only). \
                     Sessions drop on restart."
                );
                Ok(random_session_key())
            } else {
                anyhow::bail!(
                    "SESSION_KEY is required when DEV_AUTH is off. \
                     Generate one with `openssl rand -hex 64`"
                )
            }
        }
    }
}

fn random_session_key() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 64];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
