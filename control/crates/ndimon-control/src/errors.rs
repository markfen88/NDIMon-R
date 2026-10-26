//! The counterpart contract's external-tag error envelope (independent of N0).

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

pub fn error(status: StatusCode, kind: Value, message: impl Into<String>) -> Response {
    (
        status,
        Json(json!({"error": kind, "message": message.into()})),
    )
        .into_response()
}

pub fn unauthorized() -> Response {
    error(
        StatusCode::UNAUTHORIZED,
        json!("unauthorized"),
        "log in first",
    )
}

pub fn denied(message: &str) -> Response {
    error(StatusCode::FORBIDDEN, json!("permission_denied"), message)
}

pub fn invalid(field: &str, reason: &str) -> Response {
    error(
        StatusCode::BAD_REQUEST,
        json!({"invalid": {
            "field": field, "reason": reason, "errors": [{"field": field, "reason": reason}]
        }}),
        reason,
    )
}

pub fn internal() -> Response {
    error(
        StatusCode::INTERNAL_SERVER_ERROR,
        json!({"internal": {"message": "authentication storage failed"}}),
        "authentication storage failed",
    )
}
