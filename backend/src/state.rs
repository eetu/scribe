use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Instant;

use axum_extra::extract::cookie::Key;
use reqwest::Client;
use tokio::sync::Mutex;

use crate::config::Config;
use crate::db::Db;

/// In-memory map of short-lived tokens → AAXC paths on the NAS. Used to
/// hand press a one-shot URL that resolves to a local file without
/// having to expose the entire originals tree or wire shared filesystem
/// access between backend and worker. Each token is revoked the moment
/// the consuming press job completes (success or failure).
#[derive(Clone, Default)]
pub struct AaxcTokenStore {
    inner: Arc<Mutex<HashMap<String, (PathBuf, Instant)>>>,
}

impl AaxcTokenStore {
    pub async fn insert(&self, token: String, path: PathBuf) {
        self.inner.lock().await.insert(token, (path, Instant::now()));
    }
    pub async fn lookup(&self, token: &str) -> Option<PathBuf> {
        let now = Instant::now();
        let mut g = self.inner.lock().await;
        // Drop anything older than the worst-case reconvert window.
        g.retain(|_, (_, t)| now.duration_since(*t) < std::time::Duration::from_secs(3600));
        g.get(token).map(|(p, _)| p.clone())
    }
    pub async fn revoke(&self, token: &str) {
        self.inner.lock().await.remove(token);
    }
}

#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<Config>,
    pub db: Db,
    pub http: Client,
    pub cookie_key: Key,
    /// Lazy-initialised job queue. `OnceLock` because Queue construction needs
    /// an already-built AppState (it spawns workers that capture it), so we
    /// can't put the Queue inside the struct it depends on without a cycle.
    pub queue: Arc<OnceLock<crate::queue::Queue>>,
    /// Lazily-discovered OIDC provider with on-demand retry. Discovery runs
    /// on first auth use and on the `/status` poll, so a kanidm that was
    /// down at boot self-heals without a restart. See [`crate::oidc::OidcLazy`].
    pub oidc: Arc<crate::oidc::OidcLazy>,
    /// Tokens for serving local AAXC files to press during reconverts.
    pub aaxc_tokens: AaxcTokenStore,
}

impl AppState {
    pub fn queue(&self) -> &crate::queue::Queue {
        self.queue
            .get()
            .expect("queue must be initialised before first request")
    }
}

impl axum::extract::FromRef<AppState> for Key {
    fn from_ref(state: &AppState) -> Self {
        state.cookie_key.clone()
    }
}
