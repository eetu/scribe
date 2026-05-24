//! Session-based auth + profile resolution.
//!
//! Login priority (matches chat):
//!   1. OIDC configured + provider discovered → 302 to authorize URL
//!   2. `DEV_AUTH=1` → mint a session for `?username=foo[&email=…]`
//!   3. Otherwise → 503 with a hint
//!
//! Cookie payload: `sub|email` in the signed `scribe_session` cookie. The
//! `AuthProfile` extractor walks the profile table via the v2 link chain
//! (sub match → email match → auto-create) and exposes a populated
//! `Profile` to handlers.
//!
//! OIDC handshake values (csrf state, nonce, PKCE verifier, post-login
//! next URL) round-trip in a separate signed cookie `scribe_oidc` —
//! axum-extra's `SignedCookieJar` enforces integrity, and the handshake
//! cookie is removed the moment the callback consumes it.

use axum::extract::{FromRef, FromRequestParts, Query, State};
use axum::http::request::Parts;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::cookie::{Cookie, Key, SameSite, SignedCookieJar};
use openidconnect::{Nonce, PkceCodeVerifier};
use serde::Deserialize;

use crate::error::AppError;
use crate::profile::{self, Profile};
use crate::state::AppState;

const COOKIE_NAME: &str = "scribe_session";
const OIDC_COOKIE: &str = "scribe_oidc";

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
pub async fn login(
    State(state): State<AppState>,
    jar: SignedCookieJar,
    Query(q): Query<LoginQuery>,
) -> Result<Response, AppError> {
    let dest = sanitize_next(q.next.as_deref());

    // 1. OIDC if discovered.
    if let Some(oidc) = &state.oidc {
        let auth = oidc.authorize();
        // Stash the handshake values in a separate signed cookie so the
        // callback can pull them back. Format: csrf|nonce|pkce|next.
        let payload = format!(
            "{}|{}|{}|{}",
            auth.csrf.secret(),
            auth.nonce.secret(),
            auth.pkce_verifier.secret(),
            dest
        );
        let cookie = Cookie::build((OIDC_COOKIE, payload))
            .path("/")
            .http_only(true)
            .same_site(SameSite::Lax)
            .secure(false)
            .max_age(time::Duration::minutes(10))
            .build();
        return Ok((jar.add(cookie), Redirect::to(auth.url.as_str())).into_response());
    }

    // 2. DEV_AUTH fallback.
    if state.cfg.dev_auth {
        let user = q.username.unwrap_or_else(|| "dev".to_string());
        let email = q.email.unwrap_or_else(|| format!("{user}@local"));
        let _profile = profile::resolve_or_create(&state, &user, &email, None).await?;
        return Ok((write_cookie(jar, &user, &email), Redirect::to(&dest)).into_response());
    }

    // 3. Nothing configured.
    Err(AppError::BadRequest(
        "auth not configured. set DEV_AUTH=1 or all four OIDC_* env vars".into(),
    ))
}

#[derive(Debug, Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// `GET /auth/callback`
pub async fn callback(
    State(state): State<AppState>,
    jar: SignedCookieJar,
    Query(q): Query<CallbackQuery>,
) -> Result<Response, AppError> {
    let oidc = state
        .oidc
        .as_ref()
        .ok_or_else(|| AppError::BadRequest("oidc not configured".into()))?;

    if let Some(err) = &q.error {
        tracing::warn!(
            "oidc provider returned error: {err} ({:?})",
            q.error_description
        );
        return Err(AppError::BadRequest(format!(
            "provider error: {err}{}",
            q.error_description
                .as_ref()
                .map(|d| format!(" — {d}"))
                .unwrap_or_default()
        )));
    }

    let code = q
        .code
        .clone()
        .ok_or_else(|| AppError::BadRequest("missing code".into()))?;
    let returned_state = q
        .state
        .clone()
        .ok_or_else(|| AppError::BadRequest("missing state".into()))?;

    let handshake = jar
        .get(OIDC_COOKIE)
        .map(|c| c.value().to_string())
        .ok_or_else(|| AppError::BadRequest("session missing oidc handshake values".into()))?;

    // Drop the handshake cookie regardless of outcome below — replay is
    // never useful and a partial state on retry is worse than restart.
    let cleared = jar.remove(Cookie::build((OIDC_COOKIE, "")).path("/").build());

    let parts: Vec<&str> = handshake.splitn(4, '|').collect();
    if parts.len() != 4 {
        return Err(AppError::BadRequest("malformed handshake cookie".into()));
    }
    let (csrf, nonce, pkce, dest) = (parts[0], parts[1], parts[2], parts[3]);

    if csrf != returned_state {
        tracing::warn!("oidc state mismatch — possible csrf");
        return Err(AppError::BadRequest("state mismatch".into()));
    }

    let claims = oidc
        .exchange(
            &code,
            PkceCodeVerifier::new(pkce.to_string()),
            Nonce::new(nonce.to_string()),
        )
        .await
        .map_err(|e| AppError::Upstream(e.to_string()))?;

    let _profile = profile::resolve_or_create(
        &state,
        &claims.sub,
        &claims.email,
        claims.display_name.as_deref(),
    )
    .await?;

    let dest = sanitize_next(Some(dest));
    Ok((write_cookie(cleared, &claims.sub, &claims.email), Redirect::to(&dest)).into_response())
}

/// `POST /auth/logout`
pub async fn logout(jar: SignedCookieJar) -> Response {
    let cookie = Cookie::build((COOKIE_NAME, ""))
        .path("/")
        .http_only(true)
        .build();
    (jar.remove(cookie), StatusCode::NO_CONTENT).into_response()
}
