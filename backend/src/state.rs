use std::sync::Arc;
use std::sync::OnceLock;

use axum_extra::extract::cookie::Key;
use reqwest::Client;

use crate::config::Config;
use crate::db::Db;

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
    /// Discovered OIDC provider — `None` when env vars aren't set or
    /// discovery failed at boot. `/auth/login` falls back to DEV_AUTH in
    /// that case (or 503 if DEV_AUTH is off too).
    pub oidc: Option<Arc<crate::oidc::OidcContext>>,
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
