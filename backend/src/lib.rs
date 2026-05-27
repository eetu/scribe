pub mod auth;
pub mod config;
pub mod covers;
pub mod db;
pub mod error;
pub mod filenaming;
pub mod oidc;
pub mod pipeline;
pub mod poller;
pub mod press;
pub mod profile;
pub mod quality;
pub mod queue;
pub mod reconcile;
pub mod reconvert;
pub mod routes;
pub mod sidecar;
pub mod shim;
pub mod state;
pub mod sync;

use std::sync::Arc;

use tower_http::set_header::SetResponseHeaderLayer;
use tracing_subscriber::EnvFilter;

use config::Config;
use db::Db;
use state::AppState;

/// Content-Security-Policy applied to every response.
///
/// Cover art is now proxied same-origin, but `img-src` keeps the Amazon
/// hosts as a harmless fallback. `media-src` allows the optional shelf
/// origin so the library page can stream a book preview straight from
/// the shelf sidecar (cross-origin `<audio>`) — omitted when shelf is
/// unset. Everything else stays same-origin; Caddy fronts one origin in
/// prod and the vite dev server proxies api/auth/status through.
fn build_csp(shelf_url: Option<&str>) -> String {
    let media = match shelf_url {
        Some(u) => format!("media-src 'self' {}; ", u.trim_end_matches('/')),
        None => "media-src 'self'; ".to_string(),
    };
    format!(
        "default-src 'self'; \
         script-src 'self'; \
         style-src 'self' 'unsafe-inline' https://fonts.googleapis.com; \
         font-src 'self' data: https://fonts.gstatic.com; \
         img-src 'self' data: blob: https://*.media-amazon.com https://m.media-amazon.com; \
         connect-src 'self'; \
         {media}\
         frame-ancestors 'none'; \
         base-uri 'self'; \
         object-src 'none'; \
         form-action 'self'"
    )
}

pub async fn run_server() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,scribe_backend=debug")),
        )
        .init();

    let cfg = Config::from_env()?;
    let db = Db::open(&cfg.db_path)?;
    let cookie_key = auth::cookie_key(&cfg.session_key_hex);
    let http = reqwest::Client::builder()
        .user_agent(concat!("scribe/", env!("CARGO_PKG_VERSION")))
        .build()?;

    // Discover OIDC at boot if env vars present. A failure here logs and
    // falls back to DEV_AUTH path — production deploys with DEV_AUTH off
    // and discovery failing means /auth/login returns 503 until kanidm is
    // reachable + the client secret is wired.
    let oidc = match &cfg.oidc {
        Some(s) => match oidc::OidcContext::discover(s).await {
            Ok(c) => {
                tracing::info!(issuer = %s.issuer, "oidc provider discovered");
                Some(Arc::new(c))
            }
            Err(e) => {
                tracing::error!(error = %e, "oidc discovery failed; falling back to DEV_AUTH or 503");
                None
            }
        },
        None => None,
    };

    let state = AppState {
        cfg: Arc::new(cfg.clone()),
        db,
        http,
        cookie_key,
        queue: Arc::new(std::sync::OnceLock::new()),
        oidc,
        aaxc_tokens: state::AaxcTokenStore::default(),
    };

    let q = queue::Queue::new(state.clone());
    state
        .queue
        .set(q)
        .map_err(|_| anyhow::anyhow!("queue already set"))?;
    if let Err(e) = state.queue().resume_pending().await {
        tracing::warn!(error = ?e, "queue resume failed");
    }

    let csp = build_csp(cfg.shelf_url.as_deref());
    let csp_value = axum::http::HeaderValue::from_str(&csp)
        .map_err(|e| anyhow::anyhow!("invalid CSP header (bad shelf_url?): {e}"))?;
    let app = routes::router(state.clone()).layer(SetResponseHeaderLayer::if_not_present(
        axum::http::header::CONTENT_SECURITY_POLICY,
        csp_value,
    ));

    poller::spawn(state.clone());
    reconcile::spawn_boot_scan(state.clone());
    covers::spawn_boot_cache(state.clone());
    quality::spawn_boot_backfill(state.clone());

    let listener = tokio::net::TcpListener::bind(&cfg.bind).await?;
    tracing::info!(bind = %cfg.bind, dev_auth = cfg.dev_auth, "scribe listening");
    axum::serve(listener, app).await?;
    Ok(())
}
