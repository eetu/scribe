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
    let header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .ok_or(ShelfError::Unauthorized)?;
    let token = header
        .strip_prefix("Bearer ")
        .ok_or(ShelfError::Unauthorized)?;
    if !constant_time_eq(token, &state.cfg.api_key) {
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
