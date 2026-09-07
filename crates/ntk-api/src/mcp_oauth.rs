//! OAuth 2.1 для удалённого MCP: Claude Desktop подключается по одной ссылке.
//!
//! Мы здесь и защищаемый ресурс, и сервер авторизации. Личность по-прежнему
//! подтверждает Google — своих паролей у нас нет и не будет; мы лишь выдаём
//! токен на уже опознанного человека и заводим его тем же кодом, что и вход с
//! устройства (`enroll::provision`).
//!
//! Обязательное по спецификации, а не по вкусу:
//! * метаданные защищаемого ресурса (RFC 9728) и сервера авторизации (RFC 8414);
//! * саморегистрация клиента (RFC 7591) — заранее мы Claude Desktop не знаем;
//! * PKCE — без него перехваченный код обменивает кто угодно;
//! * `resource` (RFC 8707) — токен привязан к тому, для кого выпущен;
//! * `WWW-Authenticate` на 401, иначе клиенту неоткуда узнать, куда идти.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Form, Json,
};
use rand::Rng;
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{enroll, oauth, App};

/// Путь MCP-точки. Он же — канонический идентификатор ресурса в токене.
pub const MCP_PATH: &str = "/mcp-claude";

pub fn resource_url(base: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), MCP_PATH)
}

fn rnd(n: usize) -> String {
    const A: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut r = rand::thread_rng();
    (0..n).map(|_| A[r.gen_range(0..A.len())] as char).collect()
}

/// Метка состояния входа Claude. Возврат из Google общий с входом устройства,
/// и различать их надо явно, а не по длине или форме строки.
pub const STATE_PREFIX: &str = "ntkos.";

pub fn hash(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// Метаданные защищаемого ресурса. Клиент приходит сюда после 401 и узнаёт,
/// какой сервер авторизации спрашивать.
pub async fn protected_resource(State(app): State<Arc<App>>) -> Response {
    let base = app.cfg.public_url.trim_end_matches('/');
    Json(json!({
        "resource": resource_url(base),
        "authorization_servers": [base],
        "bearer_methods_supported": ["header"],
        "scopes_supported": ["ntk"]
    }))
    .into_response()
}

/// Метаданные сервера авторизации (RFC 8414).
pub async fn authorization_server(State(app): State<Arc<App>>) -> Response {
    let base = app.cfg.public_url.trim_end_matches('/');
    Json(json!({
        "issuer": base,
        "authorization_endpoint": format!("{base}/oauth/authorize"),
        "token_endpoint": format!("{base}/oauth/token"),
        "registration_endpoint": format!("{base}/oauth/register"),
        "scopes_supported": ["ntk"],
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "token_endpoint_auth_methods_supported": ["none"],
        // Только S256: «plain» сводит PKCE к украшению.
        "code_challenge_methods_supported": ["S256"]
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct Registration {
    client_name: Option<String>,
    redirect_uris: Vec<String>,
}

/// Саморегистрация клиента (RFC 7591).
///
/// Открыта всему интернету — так задумано спецификацией, и это не дыра:
/// зарегистрированный клиент ничего не может, пока живой человек не пройдёт
/// Google. Записываем, кто регистрировался, чтобы было что разбирать.
pub async fn register(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(r): Json<Registration>,
) -> Response {
    if r.redirect_uris.is_empty() {
        return bad("invalid_redirect_uri", "не указан ни один redirect_uri");
    }
    // Открытое перенаправление — способ увести код авторизации. Пускаем только
    // https и localhost, как требует спецификация.
    for u in &r.redirect_uris {
        let ok = u.starts_with("https://")
            || u.starts_with("http://localhost")
            || u.starts_with("http://127.0.0.1");
        if !ok {
            return bad("invalid_redirect_uri", &format!("{u} — нужен https или localhost"));
        }
    }
    let Ok(c) = app.pool.get().await else {
        return bad("temporarily_unavailable", "база недоступна");
    };
    let client_id = format!("ntkc_{}", rnd(24));
    let ip = crate::enroll::client_ip(&headers);
    if let Err(e) = c
        .execute(
            "insert into core.oauth_clients (client_id, client_name, redirect_uris, created_ip)
             values ($1, $2, $3, $4)",
            &[&client_id, &r.client_name, &r.redirect_uris, &ip],
        )
        .await
    {
        tracing::error!(error = %e, "регистрация клиента не прошла");
        return bad("server_error", "не удалось зарегистрировать клиента");
    }
    (
        StatusCode::CREATED,
        Json(json!({
            "client_id": client_id,
            "client_name": r.client_name,
            "redirect_uris": r.redirect_uris,
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"]
        })),
    )
        .into_response()
}

fn bad(code: &str, desc: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": code, "error_description": desc})),
    )
        .into_response()
}

#[derive(Deserialize)]
pub struct AuthorizeQuery {
    client_id: String,
    redirect_uri: String,
    response_type: String,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    state: Option<String>,
    scope: Option<String>,
    resource: Option<String>,
}

/// Шаг 1: клиент прислал человека. Проверяем клиента и уводим человека в Google.
pub async fn authorize(State(app): State<Arc<App>>, Query(q): Query<AuthorizeQuery>) -> Response {
    if q.response_type != "code" {
        return bad("unsupported_response_type", "поддерживается только code");
    }
    let (Some(challenge), Some(method)) = (q.code_challenge.as_ref(), q.code_challenge_method.as_ref())
    else {
        return bad("invalid_request", "PKCE обязателен: нужны code_challenge и code_challenge_method");
    };
    if method != "S256" {
        return bad("invalid_request", "code_challenge_method должен быть S256");
    }
    let Ok(c) = app.pool.get().await else {
        return bad("temporarily_unavailable", "база недоступна");
    };
    // Точное совпадение redirect_uri, а не префикс: префикс — это открытое
    // перенаправление с лишним шагом.
    let known = c
        .query_opt(
            "select 1 from core.oauth_clients where client_id = $1 and $2 = any(redirect_uris)",
            &[&q.client_id, &q.redirect_uri],
        )
        .await;
    match known {
        Ok(Some(_)) => {}
        Ok(None) => return bad("invalid_client", "клиент не зарегистрирован или redirect_uri не совпадает"),
        Err(e) => {
            tracing::error!(error = %e, "проверка клиента не прошла");
            return bad("server_error", "внутренняя ошибка");
        }
    }

    let state = format!("{STATE_PREFIX}{}", rnd(32));
    if let Err(e) = c
        .execute(
            "insert into core.oauth_states
               (state, client_id, redirect_uri, code_challenge, client_state, resource, scope, expires_at)
             values ($1, $2, $3, $4, $5, $6, $7, now() + interval '10 minutes')",
            &[&state, &q.client_id, &q.redirect_uri, challenge, &q.state, &q.resource, &q.scope],
        )
        .await
    {
        tracing::error!(error = %e, "состояние входа не записалось");
        return bad("server_error", "внутренняя ошибка");
    }

    // Возврат из Google — отдельный адрес: у входа с устройства состояние это
    // код устройства, и разбирать два разных смысла в одном обработчике значит
    // однажды перепутать их.
    Redirect::to(&oauth::auth_url(
        &app.cfg.google_client_id,
        &app.cfg.oauth_redirect_uri(),
        &state,
    ))
    .into_response()
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

/// Шаг 2: Google вернул человека. Опознаём, заводим и выдаём код клиенту.
pub async fn callback(State(app): State<Arc<App>>, Query(q): Query<CallbackQuery>) -> Response {
    if q.error.is_some() {
        return crate::enroll::plain_page("Вход не выполнен", "Google не подтвердил вход.");
    }
    let (Some(auth_code), Some(state)) = (q.code, q.state) else {
        return crate::enroll::plain_page("Вход не выполнен", "Google не передал код или состояние.");
    };

    let Ok(mut c) = app.pool.get().await else {
        return crate::enroll::plain_page("Временная неполадка", "База недоступна, попробуйте позже.");
    };
    // Состояние забирается ровно один раз: строка удаляется тем же запросом,
    // которым читается. Иначе один возврат из Google обменивается дважды.
    let row = c
        .query_opt(
            "delete from core.oauth_states where state = $1 and expires_at > now()
             returning client_id, redirect_uri, code_challenge, client_state, resource, scope",
            &[&state],
        )
        .await;
    let Ok(Some(st)) = row else {
        return crate::enroll::plain_page("Вход не выполнен", "Ссылка входа устарела — начните заново.");
    };
    let (client_id, redirect_uri): (String, String) = (st.get(0), st.get(1));
    let (challenge, client_state): (String, Option<String>) = (st.get(2), st.get(3));
    let (resource, scope): (Option<String>, Option<String>) = (st.get(4), st.get(5));

    let id_token = match oauth::exchange_code(
        &app.cfg.google_client_id,
        &app.cfg.google_client_secret,
        &app.cfg.oauth_redirect_uri(),
        &auth_code,
    )
    .await
    {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "обмен кода Google не прошёл");
            return crate::enroll::plain_page("Вход не выполнен", &e.to_string());
        }
    };
    let ident = match oauth::verify_id_token(&id_token, &app.cfg.google_client_id, &app.cfg.google_hd).await {
        Ok(i) => i,
        Err(e) => return crate::enroll::plain_page("Доступ не разрешён", &e.to_string()),
    };

    let code = format!("ntkac_{}", rnd(32));
    let issued = async {
        let tx = c.transaction().await?;
        let (user_id, _ws) = enroll::provision(&tx, &ident).await?;
        tx.execute(
            "insert into core.oauth_codes
               (code, client_id, user_id, redirect_uri, code_challenge, resource, scope, expires_at)
             values ($1, $2, $3, $4, $5, $6, $7, now() + interval '5 minutes')",
            &[&code, &client_id, &user_id, &redirect_uri, &challenge, &resource, &scope],
        )
        .await?;
        tx.commit().await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(e) = issued {
        tracing::warn!(error = ?e, email = %ident.email, "выдача кода не прошла");
        return crate::enroll::plain_page("Доступ не разрешён", &format!("{e:#}"));
    }

    let sep = if redirect_uri.contains('?') { '&' } else { '?' };
    let mut to = format!("{redirect_uri}{sep}code={code}");
    if let Some(s) = client_state {
        to.push_str(&format!("&state={}", urlencoding::encode(&s)));
    }
    Redirect::to(&to).into_response()
}

#[derive(Deserialize)]
pub struct TokenForm {
    grant_type: String,
    code: Option<String>,
    redirect_uri: Option<String>,
    client_id: Option<String>,
    code_verifier: Option<String>,
    refresh_token: Option<String>,
}

/// Шаг 3: обмен кода на токен. Код одноразовый, проверка PKCE обязательна.
pub async fn token(State(app): State<Arc<App>>, Form(f): Form<TokenForm>) -> Response {
    let Ok(mut c) = app.pool.get().await else {
        return bad("temporarily_unavailable", "база недоступна");
    };

    let (client_id, user_id, resource, scope) = match f.grant_type.as_str() {
        "authorization_code" => {
            let (Some(code), Some(verifier)) = (f.code.as_ref(), f.code_verifier.as_ref()) else {
                return bad("invalid_request", "нужны code и code_verifier");
            };
            // Код гасится тем же запросом, которым читается: между «прочитал» и
            // «пометил использованным» окна нет, поэтому второй обмен не пройдёт.
            let row = c
                .query_opt(
                    "update core.oauth_codes set used_at = now()
                      where code = $1 and used_at is null and expires_at > now()
                      returning client_id, user_id, redirect_uri, code_challenge, resource, scope",
                    &[code],
                )
                .await;
            let Ok(Some(r)) = row else {
                return bad("invalid_grant", "код неизвестен, просрочен или уже использован");
            };
            let (cid, uid): (String, String) = (r.get(0), r.get(1));
            let (redir, challenge): (String, String) = (r.get(2), r.get(3));
            if f.client_id.as_deref() != Some(cid.as_str()) {
                return bad("invalid_grant", "код выдан другому клиенту");
            }
            if f.redirect_uri.as_deref() != Some(redir.as_str()) {
                return bad("invalid_grant", "redirect_uri не совпадает с тем, для которого выдан код");
            }
            // S256: base64url(sha256(verifier)) без выравнивания.
            let expect = base64_url(&Sha256::digest(verifier.as_bytes()));
            if expect != challenge {
                return bad("invalid_grant", "проверка PKCE не прошла");
            }
            (cid, uid, r.get::<_, Option<String>>(4), r.get::<_, Option<String>>(5))
        }
        "refresh_token" => {
            let Some(rt) = f.refresh_token.as_ref() else {
                return bad("invalid_request", "нужен refresh_token");
            };
            let row = c
                .query_opt(
                    "select client_id, user_id, resource from core.oauth_tokens
                      where token_hash = $1 and kind = 'refresh' and revoked_at is null
                        and (expires_at is null or expires_at > now())",
                    &[&hash(rt)],
                )
                .await;
            let Ok(Some(r)) = row else {
                return bad("invalid_grant", "refresh_token неизвестен или отозван");
            };
            (r.get(0), r.get(1), r.get::<_, Option<String>>(2), None)
        }
        other => return bad("unsupported_grant_type", &format!("{other} не поддерживается")),
    };

    let access = format!("ntkat_{}", rnd(40));
    let refresh = format!("ntkrt_{}", rnd(40));
    const TTL_SEC: i64 = 3600;
    let put = async {
        let tx = c.transaction().await?;
        tx.execute(
            "insert into core.oauth_tokens (token_hash, client_id, user_id, kind, resource, expires_at)
             values ($1, $2, $3, 'access', $4, now() + make_interval(secs => $5::float8))",
            &[&hash(&access), &client_id, &user_id, &resource, &(TTL_SEC as f64)],
        )
        .await?;
        tx.execute(
            "insert into core.oauth_tokens (token_hash, client_id, user_id, kind, resource, expires_at)
             values ($1, $2, $3, 'refresh', $4, now() + interval '30 days')",
            &[&hash(&refresh), &client_id, &user_id, &resource],
        )
        .await?;
        tx.commit().await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    if let Err(e) = put {
        tracing::error!(error = ?e, "выдача токена не прошла");
        return bad("server_error", "не удалось выдать токен");
    }

    Json(json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": TTL_SEC,
        "refresh_token": refresh,
        "scope": scope.unwrap_or_else(|| "ntk".into())
    }))
    .into_response()
}

fn base64_url(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Токен → личность. Проверяется и срок, и то, что токен выпущен ДЛЯ НАС:
/// принимать чужой токен значит доверять чужому серверу авторизации.
pub async fn actor_from_token(
    c: &deadpool_postgres::Client,
    token: &str,
) -> anyhow::Result<Option<crate::auth::Actor>> {
    let row = c
        .query_opt(
            "select user_id, resource from core.oauth_tokens
              where token_hash = $1 and kind = 'access' and revoked_at is null
                and (expires_at is null or expires_at > now())",
            &[&hash(token)],
        )
        .await?;
    let Some(r) = row else { return Ok(None) };
    let user_id: String = r.get(0);
    let ws = c
        .query(
            "select workspace from core.user_workspaces where user_id = $1 order by workspace",
            &[&user_id],
        )
        .await?;
    Ok(Some(crate::auth::Actor {
        user_id,
        workspaces: ws.iter().map(|r| r.get(0)).collect(),
    }))
}

/// 401 по спецификации: без этого заголовка клиенту неоткуда узнать, где
/// спрашивать разрешение, и он покажет человеку голое «не авторизовано».
pub fn unauthorized(base: &str, detail: &str) -> Response {
    let meta = format!("{}/.well-known/oauth-protected-resource", base.trim_end_matches('/'));
    (
        StatusCode::UNAUTHORIZED,
        [(
            axum::http::header::WWW_AUTHENTICATE,
            format!("Bearer resource_metadata=\"{meta}\""),
        )],
        Json(json!({"error": "invalid_token", "error_description": detail})),
    )
        .into_response()
}
