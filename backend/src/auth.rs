//! Session-based auth + profile resolution.
//!
//! Two modes:
//!   * `DEV_AUTH=1` — `GET /auth/login?username=dev[&email=foo@bar]` writes
//!     a signed cookie directly. No OIDC. For local dev.
//!   * OIDC kanidm — `GET /auth/login` redirects to issuer, `/auth/callback`
//!     validates and writes the cookie. (Wiring lives in oidc.rs; this MVP
//!     stubs it until `OIDC_ISSUER` is populated.)
//!
//! Cookie payload: `sub|email` (pipe-separated). The `AuthProfile`
//! extractor resolves both fields, walks the profile table via the
//! v2 link chain (sub match → email match → auto-create), and exposes
//! a populated `Profile` to handlers.

use axum::extract::{FromRef, FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::cookie::{Cookie, Key, SameSite, SignedCookieJar};
use serde::Deserialize;

use crate::error::AppError;
use crate::profile::{self, Profile};
use crate::state::AppState;

const COOKIE_NAME: &str = "scribe_session";

/// Bytes extracted from the SESSION_KEY env. 64-byte minimum for signing.
pub fn cookie_key(hex: &str) -> Key {
    let bytes = hex::decode(hex).unwrap_or_else(|_| {
        let mut padded = [0u8; 64];
        for (i, b) in hex.as_bytes().iter().enumerate().take(64) {
            padded[i] = *b;
        }
        padded.to_vec()
    });
    if bytes.len() >= 64 {
        Key::from(&bytes[..64])
    } else {
        let mut padded = [0u8; 64];
        for (i, b) in bytes.iter().enumerate().take(64) {
            padded[i] = *b;
        }
        Key::from(&padded)
    }
}

#[derive(Debug, Clone)]
pub struct AuthProfile {
    pub profile: Profile,
}

impl AuthProfile {
    pub fn id(&self) -> i64 {
        self.profile.id
    }
    pub fn sub(&self) -> &str {
        self.profile.user_sub.as_deref().unwrap_or("")
    }
}

impl<S> FromRequestParts<S> for AuthProfile
where
    AppState: axum::extract::FromRef<S>,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = AppState::from_ref(state);
        let jar = SignedCookieJar::from_headers(&parts.headers, app_state.cookie_key.clone());
        let raw = jar
            .get(COOKIE_NAME)
            .map(|c| c.value().to_string())
            .ok_or(AppError::Unauthorized)?;
        let (sub, email) = parse_cookie(&raw).ok_or(AppError::Unauthorized)?;
        let profile = profile::resolve_or_create(&app_state, sub, email, None).await?;
        Ok(AuthProfile { profile })
    }
}

fn parse_cookie(raw: &str) -> Option<(&str, &str)> {
    let (sub, email) = raw.split_once('|')?;
    if sub.is_empty() || email.is_empty() {
        return None;
    }
    Some((sub, email))
}

fn write_cookie(jar: SignedCookieJar, sub: &str, email: &str) -> SignedCookieJar {
    let value = format!("{sub}|{email}");
    let cookie = Cookie::build((COOKIE_NAME, value))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(false)
        .build();
    jar.add(cookie)
}

#[derive(Debug, Deserialize)]
pub struct LoginQuery {
    username: Option<String>,
    email: Option<String>,
    /// Path the SPA wants to return to after login. Sanitized — anything
    /// not starting with `/` (or starting with `//` which would be a
    /// schemeless external URL) is rejected so this can't be used as an
    /// open-redirect.
    next: Option<String>,
}

fn sanitize_next(next: Option<&str>) -> String {
    match next {
        Some(n) if n.starts_with('/') && !n.starts_with("//") => n.to_string(),
        _ => "/".to_string(),
    }
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
    let email = q.email.unwrap_or_else(|| format!("{user}@local"));
    let dest = sanitize_next(q.next.as_deref());
    // Ensure the profile exists before handing out a cookie so a 403 fires
    // here (closed registration) rather than on the next /api/me call.
    let _profile = profile::resolve_or_create(&state, &user, &email, None).await?;
    Ok((write_cookie(jar, &user, &email), Redirect::to(&dest)).into_response())
}

/// `POST /auth/logout`
pub async fn logout(jar: SignedCookieJar) -> Response {
    let cookie = Cookie::build((COOKIE_NAME, ""))
        .path("/")
        .http_only(true)
        .build();
    (jar.remove(cookie), StatusCode::NO_CONTENT).into_response()
}
