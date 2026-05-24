use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden")]
    Forbidden,
    #[error("not found")]
    NotFound,
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("upstream: {0}")]
    Upstream(String),
    /// Audible refused to issue a voucher (410 from shim). Plus catalog
    /// rotation, cross-region denial, or otherwise unplayable. Terminal —
    /// no retry will help, and the user-facing label should explain the
    /// difference from a generic network failure.
    #[error("license denied: {0}")]
    LicenseDenied(String),
    #[error(transparent)]
    Internal(#[from] anyhow::Error),
}

impl AppError {
    fn status(&self) -> StatusCode {
        match self {
            AppError::Unauthorized => StatusCode::UNAUTHORIZED,
            AppError::Forbidden => StatusCode::FORBIDDEN,
            AppError::NotFound => StatusCode::NOT_FOUND,
            AppError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AppError::Upstream(_) => StatusCode::BAD_GATEWAY,
            AppError::LicenseDenied(_) => StatusCode::GONE,
            AppError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let body = Json(json!({
            "error": status.as_u16(),
            "detail": self.to_string(),
        }));
        if status.is_server_error() {
            tracing::error!(?self, "request failed");
        } else if matches!(
            self,
            AppError::Forbidden | AppError::Upstream(_) | AppError::LicenseDenied(_)
        ) {
            tracing::warn!(?self, "request rejected");
        }
        (status, body).into_response()
    }
}

pub type AppResult<T> = Result<T, AppError>;

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        AppError::Internal(anyhow::anyhow!(e))
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        AppError::Upstream(e.to_string())
    }
}
