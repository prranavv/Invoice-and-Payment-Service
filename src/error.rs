use axum::{
    extract::rejection::{JsonRejection, PathRejection, QueryRejection},
    http::{header::WWW_AUTHENTICATE, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

/// Every error leaves the API in one shape:
///   { "error": { "code": "<machine_readable>", "message": "<human readable>" } }
#[derive(Debug)]
pub enum AppError {
    /// 401: missing, malformed, unknown, or revoked API key (deliberately indistinguishable).
    Unauthorized,
    /// 404: also returned for resources owned by another business, so IDs can't be probed.
    NotFound(&'static str),
    /// 400: the request could not be parsed (bad JSON, bad UUID, bad query string).
    BadRequest(String),
    /// 422: the request parsed but violates a rule (empty line items, overflow, ...).
    Validation(String),
    /// Any other client-facing error with its own status and code
    /// (409 invalid_state_transition, 409 payment_in_progress, 422 idempotency_key_reuse, ...).
    Coded(StatusCode, &'static str, String),
    /// 500: details are logged, never returned to the client.
    Internal(anyhow::Error),
}

impl AppError {
    pub fn validation(msg: impl Into<String>) -> Self {
        AppError::Validation(msg.into())
    }

    /// 409 with a machine-readable code.
    pub fn conflict(code: &'static str, msg: impl Into<String>) -> Self {
        AppError::Coded(StatusCode::CONFLICT, code, msg.into())
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            AppError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "missing, malformed, or revoked API key".to_string(),
            ),
            AppError::NotFound(what) => {
                (StatusCode::NOT_FOUND, "not_found", format!("{what} not found"))
            }
            AppError::BadRequest(msg) => (StatusCode::BAD_REQUEST, "invalid_request", msg),
            AppError::Validation(msg) => {
                (StatusCode::UNPROCESSABLE_ENTITY, "validation_error", msg)
            }
            AppError::Coded(status, code, msg) => (status, code, msg),
            AppError::Internal(err) => {
                tracing::error!(error = ?err, "internal error");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal_error",
                    "internal server error".to_string(),
                )
            }
        };

        let mut response =
            (status, Json(json!({ "error": { "code": code, "message": message } }))).into_response();
        if status == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}

impl From<sqlx::Error> for AppError {
    fn from(err: sqlx::Error) -> Self {
        AppError::Internal(err.into())
    }
}

// Axum's built-in extractors reject with plain text. Mapping their rejections here
// keeps *every* error response in the JSON format above.
impl From<JsonRejection> for AppError {
    fn from(r: JsonRejection) -> Self {
        AppError::BadRequest(r.body_text())
    }
}
impl From<PathRejection> for AppError {
    fn from(r: PathRejection) -> Self {
        AppError::BadRequest(r.body_text())
    }
}
impl From<QueryRejection> for AppError {
    fn from(r: QueryRejection) -> Self {
        AppError::BadRequest(r.body_text())
    }
}