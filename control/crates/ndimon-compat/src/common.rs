//! Shared envelopes, confirmation parsing and domain CAS.
use axum::{
    Extension, Json,
    extract::rejection::JsonRejection,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use ndimon_state::{StateHandle, Submission};
use ndimon_types::*;
use serde_json::{Value, json};
pub(crate) type Body = Result<Json<Value>, JsonRejection>;
/// Set by the public authentication middleware; never a bearer credential.
#[derive(Debug, Clone)]
pub struct UiSession(pub String);
pub(crate) fn ui(session: Option<Extension<UiSession>>) -> Origin {
    Origin::Ui {
        session: session.map_or_else(|| "legacy-http".into(), |s| s.0.0),
    }
}
pub(crate) fn response(status: u16, value: Value) -> Response {
    let bytes = ndimon_render::node_json(value)
        .unwrap_or_else(|_| b"{\"ok\":false,\"error\":\"json encoding failed\"}".to_vec());
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        bytes,
    )
        .into_response()
}
pub(crate) fn success(value: Value) -> Response {
    let status = if value["ok"] == false {
        value["status"]
            .as_u64()
            .and_then(|n| u16::try_from(n).ok())
            .unwrap_or(200)
    } else {
        200
    };
    response(status, value)
}
pub(crate) fn body(value: Body) -> Result<Value, Box<Response>> {
    value
        .map(|Json(v)| v)
        .map_err(|e| Box::new(response(400, json!({"ok":false,"errors":[e.body_text()]}))))
}
pub(crate) fn request(
    command: Command,
    revision: Option<u64>,
    confirmation: &Value,
    invalid: fn(String) -> ApiError,
) -> Result<CommandRequest, ApiError> {
    let mut r = CommandRequest::new(command);
    r.expect_revision = revision;
    r.confirm = confirmation["confirm"] == true;
    if let Some(v) = confirmation.get("expect_impact") {
        r.expect_impact =
            Some(serde_json::from_value(v.clone()).map_err(|e| invalid(e.to_string()))?);
    }
    Ok(r)
}
pub(crate) fn internal_error(e: ApiError) -> Response {
    response(e.http_status(), json!(ErrorEnvelope::from(e)))
}

pub(crate) async fn submit_domain(
    h: &StateHandle,
    command: Command,
    revision: Option<u64>,
    origin: Origin,
    confirmation: &Value,
    domain: Domain,
    invalid: fn(String) -> ApiError,
) -> Result<Submission, ApiError> {
    h.submit_domain(
        request(command, revision, confirmation, invalid)?,
        origin,
        domain,
    )
    .await
}
pub(crate) fn revision_conflict(error: &ApiError, errors: Value) -> Option<Response> {
    if let ApiError::RevisionConflict { current_revision } = error {
        Some(response(
            409,
            json!({"ok":false,"status":409,"error":"revision_conflict","revision":current_revision,"errors":errors}),
        ))
    } else {
        None
    }
}
