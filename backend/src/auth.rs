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

/// Build the signing key from the SESSION_KEY hex. `config::resolve_session_key`
/// has already validated this is ≥64 bytes of real hex (or a random key in
/// dev), so decoding here is infallible — no silent zero-pad fallback that
/// would weaken a misconfigured key.
pub fn cookie_key(hex: &str) -> Key {
    let bytes = hex::decode(hex).expect("SESSION_KEY validated at config load");
    Key::from(&bytes[..64])
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
        let profile = profile::resolve_or_create(&app_state, sub, email).await?;
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

fn write_cookie(jar: SignedCookieJar, sub: &str, email: &str, secure: bool) -> SignedCookieJar {
    let value = format!("{sub}|{email}");
    let cookie = Cookie::build((COOKIE_NAME, value))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        // Secure in prod (served over HTTPS behind Caddy). Disabled only
        // under DEV_AUTH where the dev server is plain-HTTP localhost — a
        // Secure cookie would never be set there.
        .secure(secure)
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

    // 1. OIDC if configured. Discovery is lazy + retried (see OidcLazy), so
    // a kanidm that was down at boot recovers here without a restart.
    if state.oidc.is_configured() {
        match state.oidc.ctx().await {
            Some(oidc) => {
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
                    .secure(!state.cfg.dev_auth)
                    .max_age(time::Duration::minutes(10))
                    .build();
                return Ok((jar.add(cookie), Redirect::to(auth.url.as_str())).into_response());
            }
            // Configured but the issuer isn't reachable yet. In prod, surface
            // a retryable 503 rather than silently downgrading to DEV_AUTH;
            // re-discovery fires on the next /status poll or login attempt.
            // In dev, fall through to the DEV_AUTH path below.
            None if !state.cfg.dev_auth => {
                return Err(AppError::ServiceUnavailable(
                    "auth provider not reachable; retry shortly".into(),
                ));
            }
            None => {}
        }
    }

    // 2. DEV_AUTH fallback.
    if state.cfg.dev_auth {
        let user = q.username.unwrap_or_else(|| "dev".to_string());
        let email = q.email.unwrap_or_else(|| format!("{user}@local"));
        let _profile = profile::resolve_or_create(&state, &user, &email).await?;
        return Ok((
            write_cookie(jar, &user, &email, !state.cfg.dev_auth),
            Redirect::to(&dest),
        )
            .into_response());
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
    let oidc = state.oidc.ctx().await.ok_or_else(|| {
        AppError::ServiceUnavailable("auth provider not reachable; retry shortly".into())
    })?;

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

    let _profile = profile::resolve_or_create(&state, &claims.sub, &claims.email).await?;

    let dest = sanitize_next(Some(dest));
    Ok((
        write_cookie(cleared, &claims.sub, &claims.email, !state.cfg.dev_auth),
        Redirect::to(&dest),
    )
        .into_response())
}

/// `POST /auth/logout`
pub async fn logout(jar: SignedCookieJar) -> Response {
    let cookie = Cookie::build((COOKIE_NAME, ""))
        .path("/")
        .http_only(true)
        .build();
    (jar.remove(cookie), StatusCode::NO_CONTENT).into_response()
}
