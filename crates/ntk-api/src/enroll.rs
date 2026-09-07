//! Маршруты входа: устройство просит код, человек проходит Google, устройство
//! забирает ключ. Владелец в этом не участвует.

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

/// Шаг 1: расширение просит код. Секрет устройства возвращается один раз и
/// остаётся у него; по нему потом доказывается, что ключ забирает тот же, кто
/// вход начинал.
/// Адрес клиента из заголовка, который ставит Caddy. Сервис слушает только
/// localhost, поэтому иначе все запросы выглядели бы пришедшими с 127.0.0.1.
pub fn client_ip(h: &HeaderMap) -> String {
    h.get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "неизвестен".into())
}

pub async fn start(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    let ip = client_ip(&headers);
    let s = device::generate();
    let Ok(c) = app.pool.get().await else {
        return oops(StatusCode::SERVICE_UNAVAILABLE, "база недоступна");
    };

    // Просроченные коды не хранятся: они бесполезны, а копятся навсегда.
    // Уборка здесь, а не по расписанию — таблица маленькая, а лишний
    // движущийся механизм дороже одного DELETE.
    let _ = c
        .execute("delete from core.device_codes where expires_at < now() - interval '1 hour'", &[])
        .await;

    // Потолок считается ПО АДРЕСУ, а не на всех сразу.
    //
    // Глобальный потолок был дырой, которую я сам и внёс «для защиты»: двести
    // первый запрос закрывал вход ВСЕМ на пять минут, то есть один человек с
    // одной машины гасил логин всей компании. Подтверждено на живом сервере:
    // 199 успешных, дальше 51 отказ, восстановление по истечении TTL.
    //
    // По адресу распределённая попытка остаётся возможной, но это другой класс
    // и другая цена; главное — один клиент больше не отвечает за всех.
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
                "слишком много незавершённых входов с этого адреса — завершите начатый или подождите",
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
        tracing::error!(error = %e, "не удалось завести код устройства");
        return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
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

/// Шаг 3: расширение спрашивает «уже?». Ключ отдаётся РОВНО ОДИН РАЗ:
/// выдача стирается тем же запросом, которым читается.
pub async fn poll(State(app): State<Arc<App>>, Json(b): Json<PollBody>) -> Response {
    let Ok(c) = app.pool.get().await else {
        return oops(StatusCode::SERVICE_UNAVAILABLE, "база недоступна");
    };
    let code = device::normalize(&b.code);

    // Забор и стирание одной командой, но значение берётся ДО стирания.
    //
    // Здесь был дефект: `update … set issued_key = null … returning issued_key`
    // отдаёт значение ПОСЛЕ обновления, то есть уже стёртый null. Ключ
    // выдавался и терялся в тот же миг, а человек видел «ключ уже был забран».
    // В проверках путь не выполнялся: до него доходит только настоящий вход
    // через Google, машиной не проходимый. Нашлось живым пользователем.
    //
    // CTE решает обе задачи разом: `taken` читает и запирает строку, `cleared`
    // стирает её в той же команде, а наружу отдаётся прочитанное. Окна, в
    // которое ключ достался бы дважды, по-прежнему нет.
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
                None => oops(StatusCode::GONE, "ключ уже был забран"),
            }
        }
        Ok(_) => {
            // Различаем «ещё не вошли» и «просрочено»: первое лечится ожиданием.
            // «Ещё не вошли» и «ключ уже забрали» — разные ответы. Без проверки
            // claimed_at забор, случившийся в другом процессе, выглядел бы как
            // «ждите», и клиент опрашивал бы вечно.
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
                    "код просрочен, неизвестен или ключ уже забран — начните вход заново",
                )
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "опрос кода не прошёл");
            oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка")
        }
    }
}

#[derive(Deserialize)]
pub struct LinkQuery {
    code: String,
}

/// Шаг 2: человек открыл ссылку. Отправляем в Google, привязав device-код к
/// `state` — он же защищает от подстановки чужого ответа.
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

/// Экранирование для HTML.
///
/// Без него страница входа была отражённой XSS: `page()` вставляла текст в
/// разметку через format!, и `/auth/google/callback?error=<script>…` исполнялся
/// в браузере жертвы. Подтверждено на живом сервере.
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

/// То же, что `page`, но доступно другим модулям входа.
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

/// Шаг 2б: Google вернул человека. Проверяем токен, применяем правило записи,
/// выпускаем ключ и привязываем к коду. Человек ключа не видит — его заберёт
/// расширение.
pub async fn callback(State(app): State<Arc<App>>, Query(q): Query<CallbackQuery>) -> Response {
    // Тем же адресом возвращается вход Claude Desktop: список разрешённых
    // адресов живёт в консоли Google, и второй там просто так не появится.
    // Развод по явной метке, а не по догадке о форме строки.
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
        // Текст из адресной строки в страницу не попадает вовсе. Экранирование
        // уже стоит, но отражать чужой ввод незачем: чего не отражаем, тем и
        // не выстрелят.
        tracing::warn!(error = %e, "Google отказал во входе");
        return page("Вход не выполнен", "Google не подтвердил вход. Попробуйте ещё раз.");
    }
    let (Some(auth_code), Some(state)) = (q.code, q.state) else {
        return page("Вход не выполнен", "Google не передал код или состояние.");
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
            tracing::warn!(error = %e, "обмен кода не прошёл");
            return page("Вход не выполнен", &e.to_string());
        }
    };

    let ident = match oauth::verify_id_token(&id_token, &app.cfg.google_client_id, &app.cfg.google_hd).await {
        Ok(i) => i,
        Err(e) => {
            tracing::warn!(error = %e, "токен не принят");
            return page("Доступ не разрешён", &e.to_string());
        }
    };

    let Ok(mut c) = app.pool.get().await else {
        return page("Временная неполадка", "База недоступна, попробуйте позже.");
    };
    match enroll(&mut c, &ident, &device_code).await {
        Ok(ws) => page(
            "Готово",
            &format!(
                "Вы вошли как {}. Доступ: {}. Вернитесь в Claude — расширение заберёт ключ само.",
                ident.email,
                ws.join(", ")
            ),
        ),
        Err(e) => {
            // Печатаем цепочку целиком: «db error» без причины стоил живого
            // человека, который упёрся и не смог сказать, во что именно.
            tracing::warn!(error = ?e, email = %ident.email, "самозапись не прошла");
            page("Доступ не разрешён", &format!("{e:#}"))
        }
    }
}

/// Заводит человека по правилу и привязывает ключ к коду устройства.
/// Всё одной транзакцией: половина записи хуже, чем отказ целиком.
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
        anyhow::bail!("код устройства просрочен или уже использован — начните вход заново");
    }
    tx.commit().await?;
    Ok(workspaces)
}

/// Опознанный Google человек → строка в `core.users` и его воркспейсы.
///
/// Вынесено из `enroll`, потому что путей входа стало два: устройство с кодом
/// и удалённый MCP по OAuth. Второй способ заводить людей неминуемо разошёлся
/// бы с первым — в правилах записи, в проверке чужой карточки или в том, как
/// выводится идентификатор, — и разошёлся бы тихо.
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

    // Идентификатор человека — часть адреса до @. Читаемо в тикетах и совпадает
    // с тем, как люди называют друг друга.
    let user_id = email.split('@').next().unwrap_or(&email).to_string();

    // Карточка принадлежит адресу, а не совпадению инициалов.
    //
    // Идентификатор выводится из части адреса до @, а карточки заведены
    // заранее по инициалам из Notion. Без этой проверки ar@чужая-компания.com
    // получил бы карточку Александра Русских и 757 его тикетов — молча, без
    // ошибки. Инициалы совпадают легко: это вопрос времени, а не вероятности.
    //
    // Ничейная карточка (заведена администратором, адреса нет) присваивается
    // первым входом. Карточка с адресом пускает только своего владельца.
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
            // Ничейная: присваиваем. Отсюда же берётся право update(email),
            // и оно нарочно узкое — ничего другого API в карточке не меняет.
            None => {
                tx.execute("update core.users set email = $2 where id = $1", &[&user_id, &email])
                    .await?;
            }
        },
    }

    // Доступ к воркспейсам — то, ради чего правило вообще читалось.
    //
    // Раньше `workspaces` из правила только возвращались наружу, а строк в
    // core.user_workspaces не появлялось. Человек проходил Google, получал
    // ключ, страница говорила «Доступ: acme» — и `ntk whoami` отвечал
    // «воркспейсов нет». Отказ выглядел как забывчивость администратора,
    // хотя администратор был ни при чём. Проверено на живом человеке.
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
