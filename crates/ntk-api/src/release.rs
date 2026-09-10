//! Client releases: what counts as current and where to get it.
//!
//! The artefacts live in a private Spaces bucket; what goes out is not the file
//! but a pre-signed link with a short life. The Spaces keys never leave the
//! droplet and the bytes never pass through it: downloading is between the
//! client and the storage.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    Json,
};
use serde::Deserialize;

/// The client platform. Required, with no default.
///
/// darwin-arm64 used to be hard-wired here, and the server handed the macOS
/// binary to ANY client. A successful download on Linux would have replaced a
/// working binary with an incompatible one — the upgrade would have broken the
/// machine rather than fixed it.
#[derive(Deserialize)]
pub struct PlatformQuery {
    platform: Option<String>,
}

fn wanted(q: &PlatformQuery) -> Option<String> {
    q.platform.clone().filter(|p| {
        !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    })
}
use serde_json::json;

use crate::{spaces, App};

/// The current version for a platform. Answers WITHOUT a key: a client that has
/// not signed in yet must still be able to update itself, and knowing a version
/// number is not a secret.
pub async fn current(State(app): State<Arc<App>>, Query(q): Query<PlatformQuery>) -> Response {
    let Some(platform) = wanted(&q) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"name a platform — it cannot be guessed, and the wrong binary breaks the installation"})),
        )
            .into_response();
    };
    let Ok(c) = app.pool.get().await else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error":"the database is unavailable"}))).into_response();
    };
    let row = c
        .query_opt(
            "select version, object_key, sha256 from core.releases
              where platform = $1 and yanked_at is null
              order by published_at desc limit 1",
            &[&platform],
        )
        .await;

    match row {
        Ok(Some(r)) => {
            let version: String = r.get(0);
            let key: String = r.get(1);
            let sha256: String = r.get(2);
            match spaces::presign_get(&app.cfg, &key, 900) {
                Ok(url) => Json(json!({
                    "version": version,
                    "url": url,
                    "sha256": sha256,
                    "expires_in": 900
                }))
                .into_response(),
                Err(e) => {
                    tracing::error!(error = %e, "could not sign the release link");
                    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"internal error"}))).into_response()
                }
            }
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("there are no releases for platform {platform}")})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "the release query failed");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"internal error"}))).into_response()
        }
    }
}

/// Download in one link: `curl -L https://…/v1/download -o ntk`.
///
/// The bucket is private, so a bare link does not work — the server signs one
/// and redirects to it. Without this a person would have to call /v1/version
/// first, parse the JSON and only then download, and in the instructions that
/// would read as "ask someone for the file".
pub async fn download(State(app): State<Arc<App>>, Query(q): Query<PlatformQuery>) -> Response {
    let Some(platform) = wanted(&q) else {
        return (StatusCode::BAD_REQUEST, "name a platform, e.g. ?platform=darwin-arm64").into_response();
    };
    let Ok(c) = app.pool.get().await else {
        return (StatusCode::SERVICE_UNAVAILABLE, "the database is unavailable").into_response();
    };
    let row = c
        .query_opt(
            "select object_key from core.releases
              where platform = $1 and yanked_at is null
              order by published_at desc limit 1",
            &[&platform],
        )
        .await;
    match row {
        Ok(Some(r)) => match spaces::presign_get(&app.cfg, &r.get::<_, String>(0), 900) {
            Ok(url) => Redirect::temporary(&url).into_response(),
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response(),
        },
        Ok(None) => (StatusCode::NOT_FOUND, "there are no releases").into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal error").into_response(),
    }
}
