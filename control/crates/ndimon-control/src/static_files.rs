//! Public UI files. Every protected prefix that no native router claimed is a local 404.

use crate::{Shared, csrf, errors};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, header};
use axum::response::Response;
use std::path::{Component, Path};
use tower::ServiceExt;
use tower_http::services::ServeFile;

pub async fn dispatch(State(state): State<Shared>, request: Request) -> Response {
    if csrf::protected(request.uri().path()) {
        return not_found();
    }
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return not_found();
    }
    let decoded = match percent_encoding::percent_decode_str(request.uri().path()).decode_utf8() {
        Ok(decoded) => decoded.into_owned(),
        Err(_) => return errors::invalid("path", "invalid path encoding"),
    };
    let relative = Path::new(decoded.trim_start_matches('/'));
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return errors::invalid("path", "invalid static path");
    }
    let root = match tokio::fs::canonicalize(&state.config.public_dir).await {
        Ok(root) => root,
        Err(_) => return not_found(),
    };
    let mut file = root.join(relative);
    if tokio::fs::metadata(&file)
        .await
        .is_ok_and(|metadata| metadata.is_dir())
    {
        file.push("index.html");
    }
    let file = match tokio::fs::canonicalize(file).await {
        Ok(file) if file.starts_with(&root) => file,
        _ => return not_found(),
    };
    let html = file
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("html"));
    match ServeFile::new(file).oneshot(request).await {
        Ok(response) => {
            let mut response = response.map(axum::body::Body::new);
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                HeaderValue::from_static(if html {
                    "no-store"
                } else {
                    "public, max-age=86400"
                }),
            );
            response
        }
        Err(_) => errors::internal(),
    }
}

fn not_found() -> Response {
    errors::error(
        axum::http::StatusCode::NOT_FOUND,
        serde_json::json!({"not_found": {"what": "path"}}),
        "not found",
    )
}
