//! HTTP error mapping for the read-only `/api` layer.
//!
//! The MCP layer turns orchestrator errors into MCP error strings; the web
//! layer needs HTTP status codes instead, so this is a fresh mapping (it does
//! not reuse the MCP string formatters). Every error renders as
//! `{"error": "<message>"}` with the mapped status.
//!
//! Note: an unreachable embedder is **not** an error — `search_thoughts`
//! soft-fails (vector leg skipped, `vector_search_available:false`) and returns
//! `Ok`. There is therefore no 503 path here; the UI surfaces the soft-fail via
//! that flag.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use kengram_mcp::{ReadError, RelateError};

/// A read-API error: an HTTP status plus a human-readable message.
#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub message: String,
}

impl ApiError {
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(serde_json::json!({ "error": self.message })),
        )
            .into_response()
    }
}

impl From<ReadError> for ApiError {
    fn from(e: ReadError) -> Self {
        match e {
            ReadError::NotFound => ApiError {
                status: StatusCode::NOT_FOUND,
                message: e.to_string(),
            },
            ReadError::EmptyQuery
            | ReadError::LimitOutOfBounds { .. }
            | ReadError::ScopeAndPrefixBothSet => ApiError::bad_request(e.to_string()),
            ReadError::Storage(inner) => {
                tracing::error!(error = %inner, "web /api read storage error");
                ApiError {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    message: "internal database error".to_string(),
                }
            }
        }
    }
}

impl From<RelateError> for ApiError {
    fn from(e: RelateError) -> Self {
        match e {
            RelateError::ThoughtNotFound(_) => ApiError {
                status: StatusCode::NOT_FOUND,
                message: e.to_string(),
            },
            RelateError::UnknownTargetKind(_) => ApiError::bad_request(e.to_string()),
            RelateError::Storage(inner) => {
                tracing::error!(error = %inner, "web /api relate storage error");
                ApiError {
                    status: StatusCode::INTERNAL_SERVER_ERROR,
                    message: "internal database error".to_string(),
                }
            }
        }
    }
}
