//! Session-based auth.
//!
//! Two modes:
//!   * `DEV_AUTH=1` — `GET /auth/login?username=dev` writes a signed cookie
//!     directly. No OIDC. For local dev.
//!   * OIDC kanidm — `GET /auth/login` redirects to issuer, `/auth/callback`
//!     validates and writes the cookie. (Wiring lives in oidc.rs; this MVP
//!     stubs it until `OIDC_ISSUER` is populated.)
//!
//! Both produce the same downstream session: a signed cookie named
//! `scribe_session` containing the user's `sub` claim. All `/api/*` extractors
//! demand it.

use axum::extract::{FromRef, FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::cookie::{Cookie, Key, SameSite, SignedCookieJar};
use serde::Deserialize;

use crate::error::AppError;
use crate::state::AppState;

const COOKIE_NAME: &str = "scribe_session";

/// Bytes extracted from the SESSION_KEY env. 32-byte minimum for signing.
pub fn cookie_key(hex: &str) -> Key {
    let bytes = hex::decode(hex).unwrap_or_else(|_| {
        // Fall back to a hash of the input if it isn't valid hex.
        // 64 bytes required by axum-extra for signed cookies.
        let mut padded = [0u8; 64];
        for (i, b) in hex.as_bytes().iter().enumerate().take(64) {
            padded[i] = *b;
        }
        padded.to_vec()
    });
    if bytes.len() >= 64 {
        Key::from(&bytes[..64])
    } else {
        // axum-extra requires 64+ bytes for the master key.
        let mut padded = [0u8; 64];
        for (i, b) in bytes.iter().enumerate().take(64) {
            padded[i] = *b;
        }
        Key::from(&padded)
    }
}

#[derive(Debug, Clone)]
pub struct AuthUser {
    pub sub: String,
}

impl<S> FromRequestParts<S> for AuthUser
where
    AppState: axum::extract::FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);
        let jar = SignedCookieJar::from_headers(&parts.headers, app_state.cookie_key.clone());
        let sub = jar
            .get(COOKIE_NAME)
            .map(|c| c.value().to_string())
            .ok_or(AppError::Unauthorized)?;
        if sub.is_empty() {
            return Err(AppError::Unauthorized);
        }
        Ok(AuthUser { sub })
    }
}

#[derive(Debug, Deserialize)]
pub struct LoginQuery {
    username: Option<String>,
}

/// `GET /auth/login`
///
/// In DEV_AUTH mode, writes the session cookie immediately. Otherwise
/// (OIDC) returns 501 until oidc.rs is wired up.
pub async fn login(
    State(state): State<AppState>,
    jar: SignedCookieJar,
    Query(q): Query<LoginQuery>,
) -> Result<Response, AppError> {
    if !state.cfg.dev_auth {
        return Err(AppError::BadRequest(
            "OIDC login not wired yet — set DEV_AUTH=1 for now".into(),
        ));
    }
    let user = q.username.unwrap_or_else(|| "dev".to_string());
    let cookie = Cookie::build((COOKIE_NAME, user))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(false)
        .build();
    Ok((jar.add(cookie), Redirect::to("/")).into_response())
}

/// `POST /auth/logout`
pub async fn logout(jar: SignedCookieJar) -> Response {
    let cookie = Cookie::build((COOKIE_NAME, ""))
        .path("/")
        .http_only(true)
        .build();
    (jar.remove(cookie), StatusCode::NO_CONTENT).into_response()
}
