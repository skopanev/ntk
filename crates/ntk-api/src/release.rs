//! Выпуски клиента: что считать текущим и откуда его взять.
//!
//! Артефакты лежат в приватном бакете Spaces; наружу отдаётся не сам файл, а
//! предподписанная ссылка с коротким сроком. Ключи Spaces не покидают дроплет,
//! а байты не идут через него: скачивание — дело клиента и хранилища.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Redirect, Response},
    Json,
};
use serde::Deserialize;

/// Платформа клиента. Обязательна и не имеет значения по умолчанию.
///
/// Раньше здесь было зашито darwin-arm64, и сервер отдавал macOS-бинарь ЛЮБОМУ
/// клиенту. Успешное скачивание на Linux заменило бы рабочий бинарь
/// несовместимым — обновление сломало бы машину, а не починило.
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

/// Текущая версия для платформы. Отвечает БЕЗ ключа: клиент, который ещё не
/// вошёл, тоже должен уметь обновиться, а знание номера версии секретом не
/// является.
pub async fn current(State(app): State<Arc<App>>, Query(q): Query<PlatformQuery>) -> Response {
    let Some(platform) = wanted(&q) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"укажите platform — угадывать её нельзя, чужой бинарь ломает установку"})),
        )
            .into_response();
    };
    let Ok(c) = app.pool.get().await else {
        return (StatusCode::SERVICE_UNAVAILABLE, Json(json!({"error":"база недоступна"}))).into_response();
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
                    tracing::error!(error = %e, "не удалось подписать ссылку на выпуск");
                    (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"внутренняя ошибка"}))).into_response()
                }
            }
        }
        Ok(None) => (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("для платформы {platform} выпусков нет")})),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "запрос выпуска не прошёл");
            (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error":"внутренняя ошибка"}))).into_response()
        }
    }
}

/// Скачивание одной ссылкой: `curl -L https://…/v1/download -o ntk`.
///
/// Бакет приватный, поэтому голая ссылка не работает — сервер подписывает её и
/// переадресовывает. Без этого человеку пришлось бы сначала звать /v1/version,
/// разбирать JSON и только потом качать, и в инструкции это выглядело бы как
/// «возьми файл у Сергея».
pub async fn download(State(app): State<Arc<App>>, Query(q): Query<PlatformQuery>) -> Response {
    let Some(platform) = wanted(&q) else {
        return (StatusCode::BAD_REQUEST, "укажите platform, например ?platform=darwin-arm64").into_response();
    };
    let Ok(c) = app.pool.get().await else {
        return (StatusCode::SERVICE_UNAVAILABLE, "база недоступна").into_response();
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
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка").into_response(),
        },
        Ok(None) => (StatusCode::NOT_FOUND, "выпусков нет").into_response(),
        Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка").into_response(),
    }
}
