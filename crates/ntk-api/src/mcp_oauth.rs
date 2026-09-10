//! OAuth 2.1 for remote MCP: Claude Desktop connects through a single link.
//!
//! We are both the protected resource and the authorisation server here. The
//! identity is still confirmed by Google — we have no passwords of our own and
//! never will; we merely issue a token for an already-identified person and
//! enrol them through the same code as the device sign-in
//! (`enroll::provision`).
//!
//! Required by the specification rather than by taste:
//! * protected-resource (RFC 9728) and authorisation-server (RFC 8414) metadata;
//! * dynamic client registration (RFC 7591) — we do not know Claude Desktop in
//!   advance;
//! * PKCE — without it anyone who intercepts a code can exchange it;
//! * `resource` (RFC 8707) — the token is bound to whoever it was issued for;
//! * `WWW-Authenticate` on a 401, or the client has no way of learning where to
//!   go.

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

/// The MCP endpoint path. Also the canonical resource identifier in the token.
pub const MCP_PATH: &str = "/mcp-claude";

pub fn resource_url(base: &str) -> String {
    format!("{}{}", base.trim_end_matches('/'), MCP_PATH)
}

fn rnd(n: usize) -> String {
    const A: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
    let mut r = rand::thread_rng();
    (0..n).map(|_| A[r.gen_range(0..A.len())] as char).collect()
}

/// The marker on the Claude sign-in state. The Google callback is shared with
/// the device sign-in, and the two must be told apart explicitly rather than by
/// the length or shape of a string.
pub const STATE_PREFIX: &str = "ntkos.";

pub fn hash(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// Protected-resource metadata. The client comes here after a 401 and learns
/// which authorisation server to ask.
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

/// Authorisation-server metadata (RFC 8414).
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
        // S256 only: "plain" reduces PKCE to decoration.
        "code_challenge_methods_supported": ["S256"]
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct Registration {
    client_name: Option<String>,
    redirect_uris: Vec<String>,
}

/// Dynamic client registration (RFC 7591).
///
/// Open to the whole internet — that is how the specification intends it, and
/// it is not a hole: a registered client can do nothing until a live person
/// goes through Google. We record who registered so there is something to look
/// at afterwards.
pub async fn register(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(r): Json<Registration>,
) -> Response {
    if r.redirect_uris.is_empty() {
        return bad("invalid_redirect_uri", "не указан ни один redirect_uri");
    }
    // An open redirect is a way to walk off with an authorisation code. Only
    // https and localhost are allowed, as the specification requires.
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

/// Step 1: the client sent a person. We check the client and hand the person to Google.
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
    // An exact redirect_uri match, not a prefix: a prefix is an open redirect
    // with one extra step.
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

    // The Google callback is a separate address: for the device sign-in the
    // state IS the device code, and parsing two different meanings in one
    // handler means confusing them one day.
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

/// Step 2: Google sent the person back. We identify, enrol and issue a code.
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
    // The state is collected exactly once: the row is deleted by the same
    // query that reads it. Otherwise one Google callback is exchanged twice.
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

/// Step 3: exchanging the code for a token. The code is one-shot and the PKCE check is mandatory.
pub async fn token(State(app): State<Arc<App>>, Form(f): Form<TokenForm>) -> Response {
    let Ok(mut c) = app.pool.get().await else {
        return bad("temporarily_unavailable", "база недоступна");
    };

    let (client_id, user_id, resource, scope) = match f.grant_type.as_str() {
        "authorization_code" => {
            let (Some(code), Some(verifier)) = (f.code.as_ref(), f.code_verifier.as_ref()) else {
                return bad("invalid_request", "нужны code и code_verifier");
            };
            // The code is burnt by the same query that reads it: there is no
            // window between "read" and "marked used", so a second exchange
            // cannot go through.
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
            // S256: base64url(sha256(verifier)) without padding.
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

/// Token → identity. Both the expiry and the fact that the token was issued FOR
/// US are checked: accepting somebody else's token means trusting somebody
/// else's authorisation server.
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

/// A 401 by the book: without this header the client has no way of learning
/// where to ask for permission, and will show the person a bare
/// "unauthorised".
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
