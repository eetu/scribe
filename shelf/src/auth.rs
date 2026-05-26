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

pub async fn bearer_guard(
    State(state): State<ShelfState>,
    req: axum::extract::Request,
    next: Next,
) -> Result<Response, ShelfError> {
    // Prefer Authorization header. Fall back to ?token=... in the
    // query string because AVFoundation (and any plain <video>/<audio>
    // src) can't attach custom headers when AVURLAsset / AsyncImage
    // fetches the file — token-in-URL is the only auth channel for
    // those clients. ABS itself supports both modes for the same
    // reason.
    let header_token = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .map(str::to_string);
    let query_token = req
        .uri()
        .query()
        .and_then(|q| {
            url::form_urlencoded::parse(q.as_bytes())
                .find(|(k, _)| k == "token")
                .map(|(_, v)| v.into_owned())
        });
    let token = header_token
        .or(query_token)
        .ok_or(ShelfError::Unauthorized)?;
    if !constant_time_eq(&token, &state.cfg.api_key) {
        return Err(ShelfError::Unauthorized);
    }
    Ok(next.run(req).await)
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
