//! Sign-in routes: the device asks for a code, the person goes through Google,
//! the device collects the key. The owner takes no part in it.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    http::StatusCode,
    response::{Html, IntoResponse, Redirect, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;

use crate::{device, oauth, App};

fn oops(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({ "error": msg }))).into_response()
}

/// Step 1: the client asks for a code. The device secret is returned once and
/// stays with it; it later proves that the key is collected by whoever started
/// the sign-in.
/// The client address from the header Caddy sets. The service listens on
/// localhost only, so otherwise every request would look like it came from
/// 127.0.0.1.
pub fn client_ip(h: &HeaderMap) -> String {
    h.get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

pub async fn start(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    let ip = client_ip(&headers);
    let s = device::generate();
    let Ok(c) = app.pool.get().await else {
        return oops(StatusCode::SERVICE_UNAVAILABLE, "the database is unavailable");
    };

    // Expired codes are not kept: they are useless and pile up forever.
    // Cleaned here rather than on a schedule — the table is small, and one
    // more moving part costs more than a single DELETE.
    let _ = c
        .execute("delete from core.device_codes where expires_at < now() - interval '1 hour'", &[])
        .await;

    // The cap counts PER ADDRESS, not across everyone at once.
    //
    // A global cap was a hole I introduced myself "for protection": the two
    // hundred and first request shut sign-in for EVERYONE for five minutes,
    // so one person on one machine took down logins for the whole company.
    // Confirmed on the live server: 199 successes, then 51 refusals, recovery
    // once the TTL expired.
    //
    // Per address a distributed attempt is still possible, but that is a
    // different class and a different price; the point is that one client no
    // longer answers for everybody.
    if let Ok(r) = c
        .query_one(
            "select count(*) from core.device_codes
              where expires_at > now() and claimed_at is null and client_ip = $1",
            &[&ip],
        )
        .await
    {
        let live: i64 = r.get(0);
        if live > 20 {
            return oops(
                StatusCode::TOO_MANY_REQUESTS,
                "too many unfinished sign-ins from this address — finish the one you started, or wait",
            );
        }
    }
    let sql = "insert into core.device_codes (code, device_hash, expires_at, client_ip)
               values ($1, $2, now() + make_interval(secs => $3), $4)";
    if let Err(e) = c
        .execute(
            sql,
            &[&s.code, &device::hash(&s.device_secret), &(device::TTL_SECONDS as f64), &ip],
        )
        .await
    {
        tracing::error!(error = %e, "could not create a device code");
        return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
    }
    Json(json!({
        "code": s.code,
        "device_secret": s.device_secret,
        "verification_url": format!("{}/link?code={}", app.cfg.public_url.trim_end_matches('/'), s.code),
        "expires_in": device::TTL_SECONDS,
    }))
    .into_response()
}

#[derive(Deserialize)]
pub struct PollBody {
    code: String,
    device_secret: String,
}

/// Step 3: the client asks "ready yet?". The key is handed out EXACTLY ONCE:
/// it is erased by the very query that reads it.
pub async fn poll(State(app): State<Arc<App>>, Json(b): Json<PollBody>) -> Response {
    let Ok(c) = app.pool.get().await else {
        return oops(StatusCode::SERVICE_UNAVAILABLE, "the database is unavailable");
    };
    let code = device::normalize(&b.code);

    // Collect and erase in one command, but the value is taken BEFORE the
    // erase.
    //
    // There was a defect here: `update … set issued_key = null … returning
    // issued_key` returns the value AFTER the update, that is the null just
    // written. The key was issued and lost in the same instant, while the
    // person saw "the key has already been collected". The path never ran in
    // the tests: only a real Google sign-in reaches it, and a machine cannot
    // walk that. A live user found it.
    //
    // A CTE solves both at once: `taken` reads and locks the row, `cleared`
    // erases it in the same command, and what goes out is what was read. There
    // is still no window in which the key could be handed out twice.
    let rows = c
        .query(
            "with taken as (
                 select code, user_id, issued_key
                   from core.device_codes
                  where code = $1 and device_hash = $2
                    and expires_at > now() and issued_key is not null
                    for update
             ), cleared as (
                 update core.device_codes d
                    set issued_key = null, claimed_at = now()
                   from taken t
                  where d.code = t.code
             )
             select user_id, issued_key from taken",
            &[&code, &device::hash(&b.device_secret)],
        )
        .await;

    match rows {
        Ok(r) if !r.is_empty() => {
            let user: Option<String> = r[0].get(0);
            let key: Option<String> = r[0].get(1);
            match key {
                Some(k) => Json(json!({"status": "ready", "key": k, "user_id": user})).into_response(),
                None => oops(StatusCode::GONE, "the key has already been collected"),
            }
        }
        Ok(_) => {
            // "Not signed in yet" and "expired" are told apart: waiting cures the
            // first. "Not signed in yet" and "already collected" are different
            // answers too. Without the claimed_at check, a collection that happened
            // in another process would look like "keep waiting", and the client
            // would poll forever.
            let alive = c
                .query_one(
                    "select count(*) from core.device_codes
                      where code = $1 and device_hash = $2
                        and expires_at > now() and claimed_at is null",
                    &[&code, &device::hash(&b.device_secret)],
                )
                .await
                .map(|r| r.get::<_, i64>(0) > 0)
                .unwrap_or(false);
            if alive {
                (StatusCode::ACCEPTED, Json(json!({"status": "pending"}))).into_response()
            } else {
                oops(
                    StatusCode::GONE,
                    "the code is expired or unknown, or the key was already collected — start the sign-in again",
                )
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "polling the code failed");
            oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

#[derive(Deserialize)]
pub struct LinkQuery {
    code: String,
}

/// Step 2: the person opened the link. We send them to Google with the device
/// code tied to `state` — which also guards against somebody else's answer
/// being substituted.
pub async fn link(State(app): State<Arc<App>>, Query(q): Query<LinkQuery>) -> Response {
    let code = device::normalize(&q.code);
    Redirect::to(&oauth::auth_url(
        &app.cfg.google_client_id,
        &app.cfg.redirect_uri(),
        &code,
    ))
    .into_response()
}

#[derive(Deserialize)]
pub struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// HTML escaping.
///
/// Without it the sign-in page was a reflected XSS: `page()` interpolated text
/// into markup through format!, and `/auth/google/callback?error=<script>…` ran
/// in the victim's browser. Confirmed on the live server.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// The same as `page`, but available to the other sign-in modules.
pub fn plain_page(title: &str, body: &str) -> Response {
    page(title, body)
}

fn page(title: &str, body: &str) -> Response {
    Html(format!(
        "<!doctype html><meta charset=utf-8><title>{t}</title>\
         <body style=\"font:16px/1.5 system-ui;max-width:34rem;margin:4rem auto;padding:0 1rem\">\
         <h1 style=\"font-size:1.3rem\">{t}</h1><p>{b}</p></body>",
        t = esc(title),
        b = esc(body),
    ))
    .into_response()
}

/// Step 2b: Google sent the person back. We verify the token, apply the
/// enrolment rule, issue a key and tie it to the code. The person never sees
/// the key — the client will collect it.
pub async fn callback(State(app): State<Arc<App>>, Query(q): Query<CallbackQuery>) -> Response {
    // The Claude Desktop sign-in comes back through the same address: the
    // list of allowed addresses lives in the Google console, and a second one
    // does not appear there on its own. Told apart by an explicit marker
    // rather than by guessing at the shape of a string.
    if q.state.as_deref().is_some_and(|s| s.starts_with(crate::mcp_oauth::STATE_PREFIX)) {
        return crate::mcp_oauth::callback(
            State(app),
            Query(crate::mcp_oauth::CallbackQuery {
                code: q.code,
                state: q.state,
                error: q.error,
            }),
        )
        .await;
    }
    if let Some(e) = q.error {
        // Text from the address bar does not reach the page at all. The escaping
        // is already in place, but there is no reason to reflect somebody's
        // input: what is never reflected can never be fired.
        tracing::warn!(error = %e, "Google refused the sign-in");
        return page("Sign-in failed", "Google did not confirm the sign-in. Try again.");
    }
    let (Some(auth_code), Some(state)) = (q.code, q.state) else {
        return page("Sign-in failed", "Google returned no code or state.");
    };
    let device_code = device::normalize(&state);

    let id_token = match oauth::exchange_code(
        &app.cfg.google_client_id,
        &app.cfg.google_client_secret,
        &app.cfg.redirect_uri(),
        &auth_code,
    )
    .await
    {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(error = %e, "exchanging the code failed");
            return page("Sign-in failed", &e.to_string());
        }
    };

    let ident = match oauth::verify_id_token(&id_token, &app.cfg.google_client_id, &app.cfg.google_hd).await {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(error = %e, "the token was not accepted");
            return page("Access denied", &e.to_string());
        }
    };

    let Ok(mut c) = app.pool.get().await else {
        return page("Temporary failure", "The database is unavailable, try again later.");
    };
    match enroll(&mut c, &ident, &device_code).await {
        Ok(ws) => page(
            "Done",
            &format!(
                "You are signed in as {}. Access: {}. Go back to your client — it will collect the key itself.",
                ident.email,
                ws.join(", ")
            ),
        ),
        Err(e) => {
            // The whole chain is printed: a bare "db error" with no cause cost us a
            // real person who got stuck and could not say what on.
            tracing::warn!(error = ?e, email = %ident.email, "enrolment failed");
            page("Access denied", &format!("{e:#}"))
        }
    }
}

/// Enrols a person by rule and ties the key to the device code. All in one
/// transaction: half a write is worse than a refusal outright.
async fn enroll(
    client: &mut deadpool_postgres::Client,
    ident: &oauth::Identity,
    device_code: &str,
) -> anyhow::Result<Vec<String>> {
    let tx = client.transaction().await?;
    let (user_id, workspaces) = provision(&tx, ident).await?;
    let key = device::new_api_key();
    tx.execute(
        "insert into core.user_keys (key_hash, key_prefix, user_id, label)
         values ($1, $2, $3, 'самозапись')",
        &[&crate::auth::hash_key(&key), &key[..12].to_string(), &user_id],
    )
    .await?;

    let n = tx
        .execute(
            "update core.device_codes set user_id = $1, issued_key = $2
              where code = $3 and expires_at > now() and issued_key is null and claimed_at is null",
            &[&user_id, &key, &device_code],
        )
        .await?;
    if n == 0 {
        anyhow::bail!("the device code has expired or was already used — start the sign-in again");
    }
    tx.commit().await?;
    Ok(workspaces)
}

/// A person Google recognised → a row in `core.users` and their workspaces.
///
/// Pulled out of `enroll` because there are now two sign-in paths: a device
/// with a code, and remote MCP over OAuth. A second way of enrolling people
/// would inevitably have drifted from the first — in the enrolment rules, in
/// the check against somebody else's record, or in how the identifier is
/// derived — and it would have drifted quietly.
pub async fn provision(
    tx: &deadpool_postgres::Transaction<'_>,
    ident: &oauth::Identity,
) -> anyhow::Result<(String, Vec<String>)> {
    let email = ident.email.to_ascii_lowercase();
    let domain = email.split('@').nth(1).unwrap_or_default().to_string();

    let by_email = tx
        .query_opt(
            "select workspaces, role from core.enrollment_rules
              where match_type='email' and match_value=$1",
            &[&email],
        )
        .await?;
    let by_domain = tx
        .query_opt(
            "select workspaces, role from core.enrollment_rules
              where match_type='domain' and match_value=$1",
            &[&domain],
        )
        .await?;
    let rule = device::pick_rule(&email, by_email.as_ref(), by_domain.as_ref())?;
    let workspaces: Vec<String> = rule.get(0);
    let role: String = rule.get(1);

    // A person's identifier is the part of the address before the @. Readable
    // in tickets and matching how people address each other.
    let user_id = email.split('@').next().unwrap_or(&email).to_string();

    // A record belongs to an address, not to matching initials.
    //
    // The identifier is derived from the part of the address before the @,
    // while the records were created in advance from initials carried over
    // from the old system. Without this check, ar@some-other-company.com would
    // get another person's record and their 757 tickets — silently, with no
    // error. Initials collide easily: a question of time, not of probability.
    //
    // An unowned record (created by an administrator, no address on it) is
    // claimed by the first sign-in. A record with an address admits only its
    // owner.
    match tx
        .query_opt("select email from core.users where id = $1", &[&user_id])
        .await?
    {
        None => {
            tx.execute(
                "insert into core.users (id, display_name, kind, role, email)
                 values ($1, $2, 'human', $3, $4)",
                &[&user_id, &email, &role, &email],
            )
            .await?;
        }
        Some(row) => match row.get::<_, Option<String>>(0) {
            Some(known) if known == email => {}
            Some(known) => anyhow::bail!(
                "идентификатор {user_id} уже принадлежит {known}. \
                 Совпали инициалы — обратитесь к администратору, он разведёт учётные записи"
            ),
            // Unowned: claim it. This is where the update(email) right comes
            // from, and it is deliberately narrow — the API changes nothing else
            // on the record.
            None => {
                tx.execute("update core.users set email = $2 where id = $1", &[&user_id, &email])
                    .await?;
            }
        },
    }

    // Workspace access — the whole reason the rule was read at all.
    //
    // `workspaces` from the rule used to be returned outward and nothing
    // else; no rows appeared in core.user_workspaces. A person went through
    // Google, got a key, the page said "Access: acme" — and `ntk whoami`
    // answered "no workspaces". The refusal looked like an administrator's
    // forgetfulness, though the administrator had nothing to do with it.
    // Found on a real person.
    for ws in &workspaces {
        tx.execute(
            "insert into core.user_workspaces (user_id, workspace) values ($1, $2)
             on conflict do nothing",
            &[&user_id, ws],
        )
        .await?;
    }

    Ok((user_id, workspaces))
}
