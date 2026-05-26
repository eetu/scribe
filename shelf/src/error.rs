use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum ShelfError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("not found")]
    NotFound,
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl From<rusqlite::Error> for ShelfError {
    fn from(e: rusqlite::Error) -> Self {
        ShelfError::Internal(anyhow::anyhow!(e))
    }
}

impl From<std::io::Error> for ShelfError {
    fn from(e: std::io::Error) -> Self {
        ShelfError::Internal(anyhow::anyhow!(e))
    }
}

impl IntoResponse for ShelfError {
    fn into_response(self) -> Response {
        let status = match &self {
            ShelfError::Unauthorized => StatusCode::UNAUTHORIZED,
            ShelfError::NotFound => StatusCode::NOT_FOUND,
            ShelfError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ShelfError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if status.is_server_error() {
            tracing::error!(?self, "request failed");
        }
        let body = Json(json!({
            "error": status.as_u16(),
            "detail": self.to_string(),
        }));
        (status, body).into_response()
    }
}

pub type ShelfResult<T> = Result<T, ShelfError>;
