//! Native contract endpoints share the serialized actor and public guards.
use crate::{Shared, errors, sessions::Authenticated};
use axum::{
    Extension, Json, Router,
    extract::{
        Path, Query as Params, State,
        rejection::{JsonRejection, QueryRejection},
    },
    response::{IntoResponse, Response},
    routing::{get, post},
};
use ndimon_state::StateHandle;
use ndimon_types::{
    ApiError, Audio, Catalog, CommandRequest, Domain, ErrorEnvelope, Origin, Query,
};
use serde_json::Value;

pub fn router() -> Router<Shared> {
    Router::new()
        .route("/api/v1/state", get(state))
        .route("/api/v1/state/{domain}", get(domain))
        .route("/api/v1/observed", get(observed))
        .route("/api/v1/status", get(status))
        .route("/api/v1/catalog/{name}", get(catalog))
        .route("/api/v1/commands", post(command))
}
pub fn error(error: ApiError) -> Response {
    (
        axum::http::StatusCode::from_u16(error.http_status())
            .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR),
        Json(ErrorEnvelope::from(error)),
    )
        .into_response()
}
async fn query(handle: StateHandle, query: Query) -> Response {
    match handle.query(query).await {
        Ok(value) => Json(value).into_response(),
        Err(e) => error(e),
    }
}
async fn state(State(handle): State<StateHandle>) -> Response {
    query(handle, Query::State).await
}
async fn observed(State(handle): State<StateHandle>) -> Response {
    query(handle, Query::Observed).await
}
async fn status(State(handle): State<StateHandle>) -> Response {
    query(handle, Query::Status).await
}
#[derive(serde::Deserialize)]
struct CatalogParams {
    output: Option<u8>,
}
async fn catalog(
    State(handle): State<StateHandle>,
    Path(name): Path<String>,
    params: Result<Params<CatalogParams>, QueryRejection>,
) -> Response {
    let Params(params) = match params {
        Ok(params) => params,
        Err(_) => return errors::invalid("output", "expected a one-based output number"),
    };
    let name = match serde_json::from_value::<Catalog>(Value::String(name)) {
        Ok(name) => name,
        Err(_) => return errors::invalid("name", "unknown catalog"),
    };
    let value = match handle
        .query(Query::Catalog {
            name,
            output: params.output,
        })
        .await
    {
        Ok(value) => value,
        Err(e) => return error(e),
    };
    if name != Catalog::AudioEndpoints {
        return Json(value).into_response();
    }
    // Reuse the endpoint projection; configuration remains in /state/audio.
    let audio: Audio = match serde_json::from_value(value["audio"].clone()) {
        Ok(audio) => audio,
        Err(e) => {
            return error(ApiError::Internal {
                message: e.to_string(),
            });
        }
    };
    Json(ndimon_state::catalog::build(
        &value["outputs"],
        value.get("tessera_status").filter(|v| !v.is_null()),
        value["tessera_enabled"] == true,
        &audio,
    ))
    .into_response()
}
async fn domain(State(handle): State<StateHandle>, Path(domain): Path<String>) -> Response {
    match serde_json::from_value::<Domain>(Value::String(domain)) {
        Ok(domain) => query(handle, Query::StateDomain { domain }).await,
        Err(_) => errors::invalid("domain", "unknown state domain"),
    }
}
async fn command(
    State(handle): State<StateHandle>,
    Extension(auth): Extension<Authenticated>,
    session: Option<Extension<ndimon_types::NetworkSessionPath>>,
    body: Result<Json<CommandRequest>, JsonRejection>,
) -> Response {
    let mut request = match body {
        Ok(Json(request)) => request,
        Err(e) => return errors::invalid("body", &e.body_text()),
    };
    request.network_session = session.map(|Extension(path)| path);
    match handle
        .submit_with_result(
            request,
            Origin::Ui {
                session: auth.session.id,
            },
        )
        .await
    {
        Ok(reply) => {
            Json(serde_json::json!({"ok":true,"revision":reply.reply.revision,"report":reply.reply.report,"result":reply.apply}))
                .into_response()
        }
        Err(e) => error(e),
    }
}
