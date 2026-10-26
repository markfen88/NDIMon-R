//! System actions keep legacy UI confirmation semantics at this adapter only.
use crate::device::legacy::{response, success};
use crate::{common::*, device};
use axum::{
    Extension, Router,
    extract::{FromRef, OriginalUri, State},
    response::Response,
    routing::{get, post},
};
use ndimon_state::StateHandle;
use ndimon_types::*;
use serde_json::{Value, json};

pub fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    StateHandle: FromRef<S>,
{
    let mut router = Router::new();
    for prefix in ["/v1/System", "/System", "/api/v1/system"] {
        for name in ["status", "version", "vaapi-info"] {
            router = router.route(&format!("{prefix}/{name}"), get(read));
        }
        for name in ["reboot", "softreboot", "restart-stack", "update"] {
            router = router.route(&format!("{prefix}/{name}"), post(write));
        }
    }
    router
}
async fn read(State(h): State<StateHandle>, OriginalUri(uri): OriginalUri) -> Response {
    let query = match uri.path().rsplit('/').next().unwrap_or("") {
        "status" => Query::Status,
        "version" => Query::Version,
        "vaapi-info" => Query::VaapiInfo,
        _ => unreachable!(),
    };
    let status_query = matches!(query, Query::Status);
    match h.query(query).await {
        Ok(v) if status_query && v["ok"] != false => crate::common::success(v),
        Ok(v) => success(v),
        Err(e) => device::error(e),
    }
}
pub fn update_command(
    value: &Value,
    keys_configured: bool,
    privd_disabled: bool,
) -> Result<Command, ApiError> {
    if !keys_configured {
        return Err(ApiError::Unsupported {
            what: ndimon_state::domains::device::system::NO_RELEASE_KEY.into(),
        });
    }
    if privd_disabled {
        return Err(ApiError::Unsupported {
            what: "signed updates require ndimon-privd".into(),
        });
    }
    let version = value["version"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 128);
    let force = value.get("force").unwrap_or(&Value::Bool(false)).as_bool();
    let (Some(version), Some(force)) = (version, force) else {
        return Err(ApiError::invalid(vec![FieldError::new(
            "version",
            "version and optional boolean force required",
        )]));
    };
    ndimon_state::domains::device::system::signed_request(version, force)?;
    Ok(Command::InstallUpdate {
        package: None,
        version: Some(version.into()),
        force,
    })
}
async fn write(
    State(h): State<StateHandle>,
    OriginalUri(uri): OriginalUri,
    session: Option<Extension<UiSession>>,
    bytes: axum::body::Bytes,
) -> Response {
    let value = if bytes.is_empty() {
        json!({})
    } else {
        match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => v,
            Err(_) => return response(400, json!({"ok":false,"error":"invalid JSON"})),
        }
    };
    let legacy = !uri.path().starts_with("/api/");
    let name = uri.path().rsplit('/').next().unwrap_or("");
    let command = match name {
        "reboot" => Command::Reboot,
        "softreboot" | "restart-stack" => Command::RestartStack,
        "update" => match update_command(
            &value,
            ndimon_state::domains::device::system::release_keys_configured(std::path::Path::new(
                "/",
            ))
            .unwrap_or(false),
            std::env::var("NDIMON_PRIVD").as_deref() == Ok("0"),
        ) {
            Ok(c) => c,
            Err(e) => {
                return if legacy {
                    device::error(e)
                } else {
                    internal_error(e)
                };
            }
        },
        _ => unreachable!(),
    };
    let request = if legacy {
        let mut request = CommandRequest::new(command);
        request.confirm = true;
        request
    } else {
        let wire = json!({"command":command,"expect_revision":value.get("expect_revision"),"expect_impact":value.get("expect_impact"),"confirm":value.get("confirm").cloned().unwrap_or(json!(false))});
        match serde_json::from_value::<CommandRequest>(wire) {
            Ok(request) => request,
            Err(e) => {
                return internal_error(ApiError::invalid(vec![FieldError::new(
                    "body",
                    e.to_string(),
                )]));
            }
        }
    };
    if !legacy {
        return match h.submit(request, ui(session)).await {
            Ok(reply) => {
                success(json!({"ok":true,"revision":reply.revision,"report":reply.report}))
            }
            Err(e) => internal_error(e),
        };
    }
    match h.submit_domain(request, ui(session), Domain::Device).await {
        Ok(s) => {
            if name == "update" {
                success(json!({"ok":true,"job":s.apply["job"]}))
            } else {
                success(json!({"ok":true}))
            }
        }
        Err(e) => device::error(e),
    }
}
