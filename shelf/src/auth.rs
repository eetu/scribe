//! Bearer-token guard for ABS-compatible routes.
//!
//! Listen This (and any ABS client) sends
//! `Authorization: Bearer <api-key>`. We compare the value to
//! `SHELF_API_KEY` in constant time and reject mismatches with 401.
//! No DB hit, no rotation logic — rotate by changing the env value
//! and restarting the service.

use axum::extract::{FromRequestParts, State};
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::Response;
use subtle::ConstantTimeEq;

use crate::error::ShelfError;
use crate::state::ShelfState;

/// Header-only guard for the JSON/metadata routes (`/api/me`,
/// `/api/libraries`, `/api/items/{id}`, …). These are all called from code
/// that can set an `Authorization` header, so the long-lived key has no
/// reason to ride in the query string — where it would land in access /
/// reverse-proxy logs and browser history.
pub async fn bearer_guard(
    State(state): State<ShelfState>,
    req: axum::extract::Request,
    next: Next,
) -> Result<Response, ShelfError> {
    let token = header_token(&req).ok_or(ShelfError::Unauthorized)?;
    authorize(&state, &token)?;
    Ok(next.run(req).await)
}

/// Guard for the audio stream route only. Accepts the token in `?token=`
/// in addition to the header, because AVFoundation (AVURLAsset) and a plain
/// `<audio>` src can't attach a custom header when fetching the media URL —
/// token-in-URL is the only auth channel those clients have. Scoped to this
/// one route so routine API calls don't leak the key into logs.
pub async fn bearer_guard_stream(
    State(state): State<ShelfState>,
    req: axum::extract::Request,
    next: Next,
) -> Result<Response, ShelfError> {
    let token = header_token(&req)
        .or_else(|| query_token(&req))
        .ok_or(ShelfError::Unauthorized)?;
    authorize(&state, &token)?;
    Ok(next.run(req).await)
}

fn header_token(req: &axum::extract::Request) -> Option<String> {
    req.headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::to_string)
}

fn query_token(req: &axum::extract::Request) -> Option<String> {
    req.uri().query().and_then(|q| {
        url::form_urlencoded::parse(q.as_bytes())
            .find(|(k, _)| k == "token")
            .map(|(_, v)| v.into_owned())
    })
}

fn authorize(state: &ShelfState, token: &str) -> Result<(), ShelfError> {
    if constant_time_eq(token, &state.cfg.api_key) {
        Ok(())
    } else {
        Err(ShelfError::Unauthorized)
    }
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    a.as_bytes().ct_eq(b.as_bytes()).into()
}

// Marker extractor — placeholder for future per-profile work; for now
// reaching this means the guard already accepted the request.
pub struct Authed;

impl<S> FromRequestParts<S> for Authed
where
    S: Send + Sync,
{
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(_: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Authed)
    }
}
