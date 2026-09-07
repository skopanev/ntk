//! Вложения. Байты идут между клиентом и Spaces напрямую, мимо дроплета.
//!
//! Ключ объекта назначает СЕРВЕР, а не клиент. Приняв ключ от клиента, мы
//! позволили бы ему перезаписать чужой объект или выйти за пределы своего
//! воркспейса — ссылка подписывается ровно на тот ключ, который в ней назван.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;

use crate::{auth, db, spaces, App};

const TTL: u32 = 900;

fn oops(c: StatusCode, m: &str) -> Response {
    (c, Json(json!({ "error": m }))).into_response()
}

async fn enter(
    app: &Arc<App>,
    headers: &HeaderMap,
    workspace: Option<&str>,
) -> Result<(deadpool_postgres::Client, auth::Actor, String), Response> {
    let Some(key) = auth::bearer(headers) else {
        return Err(oops(StatusCode::UNAUTHORIZED, "нужен заголовок Authorization: Bearer"));
    };
    let client = app.pool.get().await.map_err(|_| oops(StatusCode::SERVICE_UNAVAILABLE, "база недоступна"))?;
    let actor = match auth::resolve(&client, key).await {
        Ok(Some(a)) => a,
        Ok(None) => return Err(oops(StatusCode::UNAUTHORIZED, "ключ неизвестен или отозван")),
        Err(_) => return Err(oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка")),
    };
    let Some(ws) = workspace else {
        return Err(oops(StatusCode::BAD_REQUEST, "укажите workspace — значения по умолчанию нет"));
    };
    if !actor.may_enter(ws) {
        return Err(oops(StatusCode::FORBIDDEN, "ключ не даёт доступа к этому воркспейсу"));
    }
    Ok((client, actor, ws.to_string()))
}

#[derive(Deserialize)]
pub struct AskUpload {
    workspace: Option<String>,
    filename: String,
    size_bytes: i64,
    content_type: Option<String>,
}

/// Шаг 1: клиент просит ссылку. Отдаём ключ и подписанный PUT.
pub async fn begin(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(ticket): Path<String>,
    Json(a): Json<AskUpload>,
) -> Response {
    let (mut client, _actor, ws) = match enter(&app, &headers, a.workspace.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if a.size_bytes <= 0 || a.size_bytes > 50 * 1024 * 1024 {
        return oops(StatusCode::BAD_REQUEST, "размер вне допустимого: от 1 байта до 50 МБ");
    }
    // Имя файла в ключ не подставляем как есть: точки, слэши и обход каталога
    // приехали бы прямо в подпись. Имя хранится в базе, ключ — сгенерирован.
    let safe: String = a
        .filename
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .take(64)
        .collect();

    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
    };
    // deleted_at обязателен: убранный тикет не должен принимать вложения.
    // Без этого условия можно было получить ссылку на загрузку к удалённому.
    if tx
        .query_opt("select 1 from tickets where id = $1 and deleted_at is null", &[&ticket])
        .await
        .ok()
        .flatten()
        .is_none()
    {
        return oops(StatusCode::NOT_FOUND, "такого тикета нет");
    }
    let uniq: String = {
        use rand::Rng;
        let mut r = rand::thread_rng();
        (0..16).map(|_| char::from(b"abcdefghijklmnopqrstuvwxyz0123456789"[r.gen_range(0..36)])).collect()
    };
    let key = format!("attachments/{ws}/{ticket}/{uniq}-{safe}");

    match spaces::presign_put(&app.cfg, &key, TTL) {
        Ok(url) => Json(json!({
            "object_key": key, "url": url, "expires_in": TTL,
            "content_type": a.content_type
        }))
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "не удалось подписать ссылку на загрузку");
            oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка")
        }
    }
}

#[derive(Deserialize)]
pub struct Done {
    workspace: Option<String>,
    object_key: String,
    filename: String,
    content_type: Option<String>,
}

/// Шаг 2: клиент сообщает, что загрузил. Сервер ПРОВЕРЯЕТ объект и только
/// потом записывает — верим хранилищу, а не клиенту.
pub async fn commit(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(ticket): Path<String>,
    Json(d): Json<Done>,
) -> Response {
    let (mut client, _actor, ws) = match enter(&app, &headers, d.workspace.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    // Ключ обязан лежать внутри своего воркспейса и своего тикета: иначе
    // клиент прикрепил бы к тикету чужой объект, назвав его своим.
    let prefix = format!("attachments/{ws}/{ticket}/");
    if !d.object_key.starts_with(&prefix) {
        return oops(StatusCode::BAD_REQUEST, "ключ объекта не принадлежит этому тикету");
    }

    let head = match spaces::presign_head(&app.cfg, &d.object_key, 60) {
        Ok(u) => reqwest::Client::new().head(u).send().await,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
    };
    let (size, etag) = match head {
        Ok(r) if r.status().is_success() => (
            r.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<i64>().ok()).unwrap_or(0),
            r.headers().get("etag").and_then(|v| v.to_str().ok()).map(|v| v.trim_matches('"').to_string()),
        ),
        _ => return oops(StatusCode::BAD_REQUEST, "объекта нет в хранилище — загрузка не состоялась"),
    };
    if size <= 0 {
        return oops(StatusCode::BAD_REQUEST, "объект пуст");
    }

    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
    };
    // Тикет мог быть убран между выдачей ссылки и подтверждением: пятнадцать
    // минут — достаточный срок, чтобы это случилось.
    if tx
        .query_opt("select 1 from tickets where id = $1 and deleted_at is null", &[&ticket])
        .await
        .ok()
        .flatten()
        .is_none()
    {
        return oops(StatusCode::NOT_FOUND, "тикета нет или он убран");
    }
    let r = tx
        .execute(
            "insert into attachments (ticket_id, object_key, filename, content_type, size_bytes, etag)
             values ($1,$2,$3,$4,$5,$6) on conflict (object_key) do nothing",
            &[&ticket, &d.object_key, &d.filename, &d.content_type, &size, &etag],
        )
        .await;
    match r {
        Ok(_) if tx.commit().await.is_ok() => {
            Json(json!({"ticket": ticket, "filename": d.filename, "size_bytes": size})).into_response()
        }
        _ => oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
    }
}

#[derive(Deserialize)]
pub struct Ws {
    workspace: Option<String>,
}

/// Список вложений с временными ссылками на скачивание.
pub async fn list(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(ticket): Path<String>,
    Query(q): Query<Ws>,
) -> Response {
    let (mut client, _actor, ws) = match enter(&app, &headers, q.workspace.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
    };
    let rows = match tx
        .query(
            "select a.object_key, a.filename, a.content_type, a.size_bytes, a.uploaded_at::text
               from attachments a
               join tickets t on t.id = a.ticket_id and t.deleted_at is null
              where a.ticket_id = $1 order by a.uploaded_at",
            &[&ticket],
        )
        .await
    {
        Ok(r) => r,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
    };
    let items: Vec<_> = rows
        .iter()
        .map(|r| {
            let key: String = r.get(0);
            json!({
                "filename": r.get::<_, String>(1),
                "content_type": r.get::<_, Option<String>>(2),
                "size_bytes": r.get::<_, i64>(3),
                "uploaded_at": r.get::<_, String>(4),
                "url": spaces::presign_get(&app.cfg, &key, TTL).unwrap_or_default(),
            })
        })
        .collect();
    Json(json!({"attachments": items})).into_response()
}
