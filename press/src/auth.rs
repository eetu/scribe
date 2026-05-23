use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;

use crate::state::PressState;

pub async fn bearer_guard(
    State(state): State<PressState>,
    req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(expected_token) = state.cfg.token.as_deref() else {
        // Auth disabled in dev — let everything through.
        return Ok(next.run(req).await);
    };
    let header = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected = format!("Bearer {expected_token}");
    // Constant-time compare to avoid timing oracles on the token.
    if !constant_time_eq(header.as_bytes(), expected.as_bytes()) {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(next.run(req).await)
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
