//! Attachments. The bytes travel between the client and Spaces directly, past
//! the droplet.
//!
//! The object key is assigned by the SERVER, not the client. Accepting a key
//! from the client would let it overwrite someone else's object or step outside
//! its own workspace — a link is signed for exactly the key named in it.

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

/// Step 1: the client asks for a link. We hand back a key and a signed PUT.
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
    // The file name does not go into the key as given: dots, slashes and
    // directory traversal would ride straight into the signature. The name is
    // kept in the database; the key is generated.
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
    // deleted_at is required: a removed ticket must not accept attachments.
    // Without this condition one could get an upload link for a deleted one.
    if tx
        .query_opt("select 1 from tickets where lower(id) = lower($1) and deleted_at is null", &[&ticket])
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

/// Step 2: the client reports that it uploaded. The server VERIFIES the object
/// and only then records it — we believe the storage, not the client.
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
    // The key must sit inside its own workspace and its own ticket: otherwise
    // a client could attach someone else's object and call it its own.
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
    // The ticket may have been removed between issuing the link and the
    // confirmation: fifteen minutes is long enough for that to happen.
    if tx
        .query_opt("select 1 from tickets where lower(id) = lower($1) and deleted_at is null", &[&ticket])
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

/// The attachment list, with short-lived download links.
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
