//! Ручки записи и захвата. Без них сервис не заменяет ntk, а мигрировать на
//! то, что умеет только читать, незачем.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;

use crate::{auth, claim, db, App};

/// Хвост идентификатора: десять символов base36. Форма пришла из прежнего
/// клиента — идентификатор набирают руками и ищут по префиксу, поэтому uuid
/// сюда не годится.
fn new_tail() -> String {
    use rand::Rng;
    const ALPHABET: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut r = rand::thread_rng();
    (0..10).map(|_| ALPHABET[r.gen_range(0..ALPHABET.len())] as char).collect()
}

fn oops(c: StatusCode, m: &str) -> Response {
    (c, Json(json!({ "error": m }))).into_response()
}

/// Общее начало всех ручек: ключ, личность, воркспейс, право войти.
async fn enter(
    app: &Arc<App>,
    headers: &HeaderMap,
    workspace: Option<&str>,
) -> Result<(deadpool_postgres::Client, auth::Actor, String), Response> {
    let Some(key) = auth::bearer(headers) else {
        return Err(oops(StatusCode::UNAUTHORIZED, "an Authorization: Bearer header is required"));
    };
    let client = app
        .pool
        .get()
        .await
        .map_err(|_| oops(StatusCode::SERVICE_UNAVAILABLE, "the database is unavailable"))?;
    let actor = match auth::resolve(&client, key).await {
        Ok(Some(a)) => a,
        Ok(None) => return Err(oops(StatusCode::UNAUTHORIZED, "unknown or revoked key")),
        Err(_) => return Err(oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error")),
    };
    let Some(ws) = workspace else {
        return Err(oops(StatusCode::BAD_REQUEST, "name a workspace — there is no default"));
    };
    if !actor.may_enter(ws) {
        return Err(oops(StatusCode::FORBIDDEN, "this key gives no access to that workspace"));
    }
    Ok((client, actor, ws.to_string()))
}

#[derive(Deserialize)]
pub struct Ws {
    workspace: Option<String>,
}

/// Параметры захвата: порядок и ОТБОР, отдельными полями.
///
/// Своя структура, а не расширенная Ws: у Ws четыре других потребителя, и
/// отборы захвата им не принадлежат. Общая структура делала бы вид, что
/// `?module=` что-то значит и для них.
#[derive(Deserialize)]
pub struct NextQuery {
    workspace: Option<String>,
    /// Теги в порядке предпочтения. ПОРЯДОК, а не отбор.
    prefer: Option<String>,
    /// Отбор по тегам через запятую. Не подошло — не выдаётся вовсе.
    tag: Option<String>,
    #[serde(default)]
    strict: bool,
    project: Option<String>,
    module: Option<String>,
    /// Любой действующий модуль вместо конкретного имени.
    #[serde(default)]
    has_module: bool,
    assignee: Option<String>,
    /// Показать, что БЫ взялось, ничего не забирая.
    #[serde(default)]
    dry_run: bool,
}

/// Взять любой свободный тикет. Параллельные агенты разбирают очередь, не
/// мешая друг другу.
pub async fn next(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Query(q): Query<NextQuery>,
) -> Response {
    let (mut client, actor, ws) = match enter(&app, &headers, q.workspace.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    };
    let prefer: Vec<String> = q
        .prefer
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();

    let pick = claim::Pick {
        tags: q
            .tag
            .as_deref()
            .unwrap_or("")
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
        strict: q.strict,
        project: q.project.clone(),
        module: q.module.clone(),
        has_module: q.has_module,
        assignee: q.assignee.clone(),
        dry_run: q.dry_run,
    };

    match claim::next(&tx, &prefer, &pick).await {
        Ok(Some(c)) => {
            if tx.commit().await.is_err() {
                return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
            }
            // Захват больше не пишется в assignee — там владелец тикета, а не
            // тот, кто взял его в работу. Лог остаётся единственным следом
            // того, КТО взял, и потому обязателен.
            if q.dry_run {
                // Ни записи, ни строчки «тикет взят» в журнале: журнал —
                // единственный след того, КТО взял, и предпросмотр не должен
                // оставлять в нём ложный след.
                return Json(json!({
                    "id": c.id, "title": c.title, "status": c.status, "claimed": false
                }))
                .into_response();
            }
            tracing::info!(actor = %actor.user_id, ticket = %c.id, workspace = %ws, "тикет взят");
            Json(json!({"id": c.id, "title": c.title, "status": c.status, "claimed": true})).into_response()
        }
        Ok(None) => (StatusCode::NO_CONTENT, ()).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "next не прошёл");
            oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

/// Взять конкретный тикет. Проигравший получает 409 и объяснение, а не тишину.
pub async fn start(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<Ws>,
) -> Response {
    let (mut client, actor, ws) = match enter(&app, &headers, q.workspace.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    };
    // Идентификатор приводится к каноническому виду сразу после входа в
    // воркспейс: дальше все сравнения точные, и ни одно из них не надо помнить.
    let id = match canonical_id(&tx, &id).await {
        Some(v) => v,
        None => return oops(StatusCode::NOT_FOUND, "no such ticket"),
    };

    match claim::start(&tx, &id).await {
        Ok(claim::StartOutcome::Taken(c)) => {
            if tx.commit().await.is_err() {
                return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
            }
            tracing::info!(actor = %actor.user_id, ticket = %c.id, workspace = %ws, "тикет взят");
            Json(json!({"id": c.id, "title": c.title, "status": c.status, "claimed": true})).into_response()
        }
        Ok(claim::StartOutcome::AlreadyTaken { status, agent }) => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "the ticket is already taken",
                "status": status,
                "agent": agent
            })),
        )
            .into_response(),
        Ok(claim::StartOutcome::NoSuchTicket) => oops(StatusCode::NOT_FOUND, "no such ticket"),
        Ok(claim::StartOutcome::Blocked { deps }) => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "the ticket is blocked by unclosed dependencies",
                "blocked_by": deps.iter()
                    .map(|(id, st)| json!({"id": id, "status": st}))
                    .collect::<Vec<_>>()
            })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "start не прошёл");
            oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
        }
    }
}

/// Неизвестное поле — отказ, а не тишина.
///
/// Без этого клиент новее сервера получает «изменён» и не получает
/// изменения: сервер молча выбрасывает поле, о котором не знает. Ровно так
/// правка зависимостей ушла в никуда с успешным ответом — и это тот самый
/// класс дефекта, который мы весь проект ловим: не отказ, а тихо неверное
/// поведение. Лучше сломаться заметно.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    workspace: Option<String>,
    #[serde(default)]
    force: bool,
    status: Option<String>,
    title: Option<String>,
    body: Option<String>,
    assignee: Option<String>,
    /// Правки тегов, каждая со знаком: ["+alpha","-legacy"].
    ///
    /// Именно правки, а не замена: в старом инструменте тег без знака
    /// отвергался, потому что «добавить» однажды оказалось бы «заменить всё»,
    /// и тикет молча терял бы историю пометок.
    tag_edits: Option<Vec<String>>,
    /// Дописать в конец тела, не трогая написанное.
    ///
    /// Отдельно от `body`, а не флагом рядом с ним: замена и дописывание —
    /// разные намерения, и перепутать их значит стереть чужой разбор. В старом
    /// инструменте это были `-d` и `-A`, и он же запрещал ставить их вместе.
    ///
    /// Склейка идёт в базе одним UPDATE. Читать тело, склеивать у клиента и
    /// писать обратно — это ровно та гонка, из-за которой мы уехали с Notion:
    ///два дописывания подряд, и одно пропадает без единой ошибки.
    body_append: Option<String>,
    /// Правки зависимостей, каждая со знаком: ["+proj-a1b2c3","-proj-d4e5f6"].
    ///
    /// Со знаком по той же причине, что и теги: голый список однажды означал
    /// бы «заменить все», и тикет молча терял бы связи, которых никто не
    /// называл. Цель обязана существовать — ребро в никуда превращает «жду
    /// такой-то тикет» в «ничего не жду», и об этом никто не узнаёт.
    dep_edits: Option<Vec<String>>,
    /// Поля, потерянные при переписывании с Notion-версии. Приоритет тикета
    /// изменить было НЕЛЬЗЯ вообще — а это первое, что правят, когда работа
    /// оказывается срочнее, чем думали.
    priority: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    project: Option<String>,
    /// Срок. Пустая строка снимает его: в старом инструменте это была
    /// единственная возможность убрать дату, и её надо сохранить.
    due: Option<String>,
    /// Замена всего набора зависимостей — форма `a,b` старого `--deps`.
    /// Отдельным полем, а не смешением с dep_edits: там знак обязателен, и
    /// два разных намерения в одном поле — ровно то, на чём мы обожглись с
    /// тегами.
    dep_set: Option<Vec<String>>,
    /// Модуль — единица работы внутри проекта. Пустая строка снимает его.
    module: Option<String>,
}

/// Правка тикета. Гард: всё, что не в группе `todo`, требует `force` — тикет
/// уже кем-то подобран. Политика читается из таблицы, а не зашита здесь.
pub async fn patch(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(p): Json<Patch>,
) -> Response {
    let (mut client, actor, ws) = match enter(&app, &headers, p.workspace.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    // Замена и дописывание вместе — почти наверняка описка, и цена описки тут
    // чужой разбор в теле тикета. Старый инструмент отвергал это же сочетание.
    if p.body.is_some() && p.body_append.is_some() {
        return oops(
            StatusCode::BAD_REQUEST,
            "body and body_append are not accepted together: either replace the body or append to it",
        );
    }
    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    };
    // Идентификатор приводится к каноническому виду сразу после входа в
    // воркспейс: дальше все сравнения точные, и ни одно из них не надо помнить.
    let id = match canonical_id(&tx, &id).await {
        Some(v) => v,
        None => return oops(StatusCode::NOT_FOUND, "no such ticket"),
    };


    let Ok(Some(row)) = tx.query_opt("select status from tickets where id = $1 and deleted_at is null", &[&id]).await else {
        return oops(StatusCode::NOT_FOUND, "no such ticket");
    };
    let current: String = row.get(0);

    // Moving work forward does not need force, and that is the point of the
    // guard rather than a hole in it.
    //
    // The guard exists so nobody edits SOMEONE ELSE'S work mid-flight. But
    // carrying your own started work to the next stage is not an edit — it is
    // the ordinary path every ticket takes. Demanding force here would mean
    // setting it out of routine, and a routine "do it anyway" flag stops
    // meaning anything.
    //
    // The exemption is NARROW: only when the status is the single field
    // changing, and only for statuses marked `free_target`. Closing while also
    // rewriting someone else's body still needs force.
    //
    // Which statuses those are is DATA, not code (see sql/027): the owner adds
    // one with an UPDATE. It used to be a hardcoded check for the terminal
    // group, and `to_test` therefore demanded force to hand work to a tester.
    let only_status = p.status.is_some()
        && p.title.is_none()
        && p.body.is_none()
        && p.body_append.is_none()
        && p.assignee.is_none()
        && p.tag_edits.is_none()
        && p.dep_edits.is_none()
        && p.dep_set.is_none()
        && p.priority.is_none()
        && p.kind.is_none()
        && p.project.is_none()
        && p.due.is_none()
        && p.module.is_none();
    let moving_on = only_status
        && match p.status.as_deref() {
            Some(target) => tx
                .query_opt(
                    "select 1 from statuses where name = $1 and free_target",
                    &[&target],
                )
                .await
                .ok()
                .flatten()
                .is_some(),
            None => false,
        };

    if !p.force && !moving_on {
        match claim::requires_force(&tx, &current).await {
            Ok(true) => {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "error": format!("a ticket in status {current} is already picked up — force is required"),
                        "status": current
                    })),
                )
                    .into_response()
            }
            Ok(false) => {}
            Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        }
    }

    if let Some(edits) = &p.tag_edits {
        for e in edits {
            let (sign, name) = e.split_at(1);
            let name = name.trim();
            if name.is_empty() || !matches!(sign, "+" | "-") {
                return oops(StatusCode::BAD_REQUEST, "every tag must start with + or -");
            }
            let sql = if sign == "+" {
                "update tickets set tags = (select array_agg(distinct t) from unnest(tags || $2::text) t) where id = $1"
            } else {
                "update tickets set tags = array_remove(tags, $2) where id = $1"
            };
            if tx.execute(sql, &[&id, &name]).await.is_err() {
                return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
            }
        }
    }

    if let Some(set) = &p.dep_set {
        if tx.execute("delete from deps where ticket_id = $1", &[&id]).await.is_err() {
            return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
        }
        for target in set {
            let target = target.trim();
            if target.is_empty() { continue; }
            if target.eq_ignore_ascii_case(&id) {
                return oops(StatusCode::BAD_REQUEST, "a ticket cannot wait for itself");
            }
            if would_cycle(&tx, &id, target).await {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error": format!(
                        "{target} already waits for {id}, directly or through others: \
                         this would close a ring, and nothing in a ring can ever be unblocked"
                    )})),
                )
                    .into_response();
            }
            match tx
                .execute(
                    "insert into deps (ticket_id, depends_on)
                     select $1, id from tickets where lower(id) = lower($2) and deleted_at is null
                     on conflict do nothing",
                    &[&id, &target],
                )
                .await
            {
                Ok(0) => {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": format!("no such ticket: {target}")})),
                    )
                        .into_response()
                }
                Ok(_) => {}
                Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
            }
        }
    }

    if let Some(edits) = &p.dep_edits {
        for e in edits {
            if e.len() < 2 {
                return oops(StatusCode::BAD_REQUEST, "every dependency must start with + or -");
            }
            let (sign, target) = e.split_at(1);
            let target = target.trim();
            if target.is_empty() || !matches!(sign, "+" | "-") {
                return oops(StatusCode::BAD_REQUEST, "every dependency must start with + or -");
            }
            if sign == "+" {
                if target.eq_ignore_ascii_case(&id) {
                    return oops(StatusCode::BAD_REQUEST, "a ticket cannot wait for itself");
                }
                if would_cycle(&tx, &id, target).await {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": format!(
                            "{target} already waits for {id}, directly or through others: \
                             this would close a ring, and nothing in a ring can ever be unblocked"
                        )})),
                    )
                        .into_response();
                }
                // Вставка идёт SELECT-ом по существующему тикету: если цели
                // нет, строк будет ноль, и мы скажем об этом вместо того,
                // чтобы завести ребро в пустоту.
                match tx
                    .execute(
                        "insert into deps (ticket_id, depends_on)
                         select $1, id from tickets where lower(id) = lower($2) and deleted_at is null
                         on conflict do nothing",
                        &[&id, &target],
                    )
                    .await
                {
                    Ok(0) => {
                        // Ноль строк — либо цели нет, либо ребро уже стоит.
                        // Различаем: повтор не ошибка, отсутствие цели — да.
                        let exists = tx
                            .query_opt("select 1 from tickets where lower(id) = lower($1) and deleted_at is null", &[&target])
                            .await;
                        if matches!(exists, Ok(None)) {
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(json!({"error": format!("no such ticket: {target}")})),
                            )
                                .into_response();
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
                }
            } else if tx
                .execute(
                    "delete from deps where ticket_id = $1 and lower(depends_on) = lower($2)",
                    &[&id, &target],
                )
                .await
                .is_err()
            {
                return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
            }
        }
    }

    // Смена проекта у тикета с модулем: модуль назначения обязан быть назван
    // в этой же правке. Молча снять его нельзя — тикет потерял бы
    // принадлежность, о которой никто не просил; оставить как есть нельзя
    // тоже — внешний ключ на пару отвергнет запись, и человек увидит
    // «внутреннюю ошибку» вместо объяснения.
    if p.project.is_some() && p.module.is_none() {
        let current: Option<Option<String>> = tx
            .query_opt("select module from tickets where id = $1", &[&id])
            .await
            .ok()
            .flatten()
            .map(|r| r.get(0));
        if matches!(current, Some(Some(_))) {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": "the ticket has a module: when changing project, name a module of the new project or clear it with an empty string"})),
            )
                .into_response();
        }
    }

    if let Some(m) = p.module.as_deref().filter(|m| !m.trim().is_empty()) {
        // Проект берётся из правки, а если он не меняется — из самого тикета.
        let target_project: Option<String> = match p.project.clone() {
            Some(pr) => Some(pr),
            None => tx
                .query_opt("select project_id from tickets where id = $1", &[&id])
                .await
                .ok()
                .flatten()
                .and_then(|r| r.get(0)),
        };
        if module_is_archived(&tx, target_project.as_deref(), m).await {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("module {m} is archived — a ticket cannot be moved onto it")})),
            )
                .into_response();
        }
    }

    if let Some(proj) = p.project.as_deref() {
        if tx
            .execute("insert into projects (id, name) values ($1, $1) on conflict (id) do nothing", &[&proj])
            .await
            .is_err()
        {
            return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
        }
    }

    // Пределы длины при правке. Прежняя длина нужна затем, чтобы уже записанное
    // сверх предела не стало нередактируемым: старые тикеты никто не переписывает,
    // и запрет их править заморозил бы их навсегда. Отказ приходит только когда
    // значение превышает предел И РАСТЁТ — то есть «старых не трогаем, но и
    // раздувать дальше не даём».
    let prev: Option<(String, String)> = tx
        .query_opt("select title, body from tickets where id = $1", &[&id])
        .await
        .ok()
        .flatten()
        .map(|r| (r.get(0), r.get(1)));

    if let Some(t) = &p.title {
        let was = prev.as_ref().map(|(ti, _)| ti.chars().count());
        if let Some(m) = check_len(&tx, "title", t, was).await {
            return oops(StatusCode::BAD_REQUEST, &m);
        }
    }
    // Замена и дописывание считаются одинаково — по ИТОГОВОЙ длине. Иначе
    // дописывание стало бы обходом предела: по строчке за раз до трёхсот тысяч.
    let was_body = prev.as_ref().map(|(_, b)| b.chars().count());
    if let Some(b) = &p.body {
        if let Some(m) = check_len(&tx, "body", b, was_body).await {
            return oops(StatusCode::BAD_REQUEST, &m);
        }
    }
    if let Some(a) = &p.body_append {
        if let Some(max) = write_limit(&tx, "body").await {
            let max = max as usize;
            let was = was_body.unwrap_or(0);
            let add = a.chars().count();
            if was > max {
                // Тикет УЖЕ сверх предела — он из времён до предела. Дописать в
                // него короткую ссылку на доказательство можно: запрет заморозил
                // бы старое навсегда, а читаемость такого тикета короткая пометка
                // не ухудшает. Но за один раз нельзя добавить больше предела —
                // иначе дописывание снова становится способом свалить сюда сто
                // тысяч знаков, только по частям.
                if add > max {
                    return oops(
                        StatusCode::BAD_REQUEST,
                        &format!(
                            "the ticket is already over the limit ({was} against {max}); appending is allowed, \
                             but not more than {max} characters at a time — {add} arrived. Split it into \
                             separate tickets and link them with --deps."
                        ),
                    );
                }
            } else if was + add > max {
                return oops(
                    StatusCode::BAD_REQUEST,
                    &format!(
                        "the body after appending is over the limit: {} characters against {max}. \
                         Split the work into separate tickets and link them with --deps.",
                        was + add
                    ),
                );
            }
        }
    }

    // Пустая строка в due означает «снять срок», а не «не трогать»: в старом
    // инструменте это была единственная возможность убрать дату.
    let clear_due = p.due.as_deref().is_some_and(|d| d.trim().is_empty());
    let due_value = if clear_due { None } else { p.due.clone() };
    let clear_module = p.module.as_deref().is_some_and(|m| m.trim().is_empty());
    let module_value = if clear_module { None } else { p.module.clone() };

    let r = tx
        .execute(
            "update tickets set
               status     = coalesce($2, status),
               title      = coalesce($3, title),
               body       = case when $6::text is not null
                                then coalesce($4, body) || $6::text
                                else coalesce($4, body) end,
               assignee   = coalesce($5, assignee),
               priority   = coalesce($7, priority),
               type       = coalesce($8, type),
               project_id = coalesce($9, project_id),
               module     = case when $12 then null else coalesce($13, module) end,
               due        = case when $11 then null else coalesce($10::text::date, due) end
             where id = $1",
            &[&id, &p.status, &p.title, &p.body, &p.assignee, &p.body_append,
              &p.priority, &p.kind, &p.project, &due_value, &clear_due,
              &clear_module, &module_value],
        )
        .await;
    // Долг — до commit и в той же транзакции. Правка, не изменившая
    // отправляемый текст (приоритет, исполнитель, срок), долга не создаёт.
    if let (Ok(_), Err(e)) = (&r, enqueue_upsert(&tx, &id).await) {
        tracing::error!(error = %e, ticket = %id, workspace = %ws, "долг индексации не поставлен");
        return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
    }
    match r {
        Ok(_) if tx.commit().await.is_ok() => {
            tracing::info!(actor = %actor.user_id, ticket = %id, workspace = %ws, forced = p.force, "тикет изменён");
            crate::vector::wake(&app);
            Json(json!({"id": id, "updated": true})).into_response()
        }
        // Нарушенное ограничение — ошибка ВЫЗЫВАЮЩЕГО, а не сбой сервиса.
        //
        // Раньше здесь любая неудача сводилась к «внутренняя ошибка» с кодом
        // 500: неизвестный исполнитель или незаведённый модуль выглядели как
        // поломка сервера, и найти причину было нечем — ни поля, ни намёка.
        Err(e) => {
            let db = e.as_db_error();
            let constraint = db.and_then(|d| d.constraint()).unwrap_or_default();
            tracing::error!(
                error = %e,
                constraint = %constraint,
                detail = %db.map(|d| d.message()).unwrap_or_default(),
                ticket = %id, workspace = %ws,
                "не удалось изменить тикет"
            );
            match explain_constraint(constraint) {
                Some(m) => oops(StatusCode::BAD_REQUEST, &m),
                None => oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
            }
        }
        _ => oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    workspace: Option<String>,
    /// Идентификатор назначает СЕРВЕР. Поле оставлено ради переноса данных:
    /// импорт обязан сохранить те идентификаторы, что уже вшиты в сообщения
    /// коммитов, а обычное заведение тикета его не присылает.
    ///
    /// Раньше придумывать его был обязан клиент — наследие Notion, где id
    /// генерировал JS-клиент. Стоило это дорого: MCP-инструмент `ntk_create`
    /// поля не слал, сервер отвечал отказом разбора, и заведение тикета через
    /// MCP не работало вовсе. Знание о форме идентификатора должно жить в
    /// одном месте, и это место — сервер.
    #[serde(default)]
    id: Option<String>,
    title: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    priority: Option<String>,
    /// На проводе поле зовётся `type`, как и при правке (см. Patch), а `kind`
    /// принимается как псевдоним.
    ///
    /// Псевдоним не вкусовщина: структура объявлена deny_unknown_fields, и
    /// клиент, приславший не то имя, получает отказ РАЗБОРА — наружу это
    /// выглядит как «ответ сервиса не разобрался», без единого намёка на
    /// причину. Так и случилось: заведение принимало `kind`, правка — `type`,
    /// а MCP слал `type` в обе. Уже выпущенные клиенты шлют `kind`, поэтому
    /// одним переименованием чинить нельзя: сломались бы они.
    #[serde(default, rename = "type", alias = "kind")]
    kind: Option<String>,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    deps: Vec<String>,
    module: Option<String>,
    /// Не искать похожих перед заведением.
    ///
    /// По умолчанию заведение СНАЧАЛА ищет тикеты про то же самое и отказывает,
    /// если находит: агент, заводящий дубль, обычно не знает, что дубль, и
    /// сказать ему об этом можно только в ответе на его же действие. Флаг —
    /// осознанный обход, а не удобство.
    #[serde(default)]
    skip_search: bool,
}

/// Заведение тикета.
///
/// Занятый идентификатор — это 409, а не молчаливая перезапись: `on conflict
/// do nothing` вернёт ноль строк, и клиент сгенерирует другой хвост и
/// повторит. Проверка коллизий чтением всей базы уходит вместе с Notion — она
/// стоила 26 запросов и всё равно имела окно гонки между чтением и записью.
/// Запрос похожих. Текст приходит телом, а не в строке запроса: тело тикета
/// доходит до двух тысяч символов, и в URL ему не место.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SimilarQ {
    workspace: Option<String>,
    /// Искомый текст. Заголовок, фраза или тикет целиком — всё равно, в модель
    /// уходит склейка, обрезанная до предела.
    #[serde(default, alias = "title")]
    text: Option<String>,
    #[serde(default)]
    body: Option<String>,
    /// Искать похожих на УЖЕ существующий тикет.
    #[serde(default)]
    id: Option<String>,
    /// Отбор — тот же, что у списка: статусы через запятую, теги, исполнитель,
    /// проект, модуль. По умолчанию ищем во ВСЕХ статусах и у всех.
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    tag: Option<String>,
    #[serde(default)]
    strict: bool,
    #[serde(default)]
    assignee: Option<String>,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    module: Option<String>,
    #[serde(default)]
    limit: Option<i64>,
    #[serde(default)]
    min_score: Option<f64>,
}

/// Сколько похожих отдаём максимум. Двадцать — уже не «похожие», а полки.
const SIMILAR_MAX: usize = 20;

/// Похожие тикеты.
///
/// Отказ, а не пустой список, когда векторизация выключена: пустой список
/// читается как «ничего похожего нет», то есть как разрешение заводить. Разница
/// между «искал и не нашёл» и «не искал» здесь стоит дубля в очереди.
pub async fn similar(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(p): Json<SimilarQ>,
) -> Response {
    let (mut client, _actor, ws) = match enter(&app, &headers, p.workspace.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let Some(v) = app.vector.clone() else {
        return oops(
            StatusCode::CONFLICT,
            "the similarity search is unavailable: the service has no embedding key",
        );
    };
    let mut policy_min = crate::vector::NEAR_DUPLICATE;

    // Текст: либо присланный, либо взятый у существующего тикета.
    let (text, body, exclude) = if let Some(id) = p.id.as_deref() {
        if p.text.is_some() || p.body.is_some() {
            return oops(
                StatusCode::BAD_REQUEST,
                "id and text are not accepted together: either similar to a ticket, or similar to the text you sent",
            );
        }
        let tx = match db::begin(&mut client, &ws).await {
            Ok(t) => t,
            Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };
        let enabled = match vector_enabled(&tx).await {
            Ok(e) => e,
            Err(e) => {
                tracing::error!(error = %e, workspace = %ws, "vector policy did not read");
                return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
            }
        };
        if !enabled {
            return oops(
                StatusCode::CONFLICT,
                "vectorisation is switched off in this workspace: there is nothing to search",
            );
        }
        policy_min = vector_stop_policy(&tx).await.map(|(_, m)| m).unwrap_or(policy_min);
        let canonical = match canonical_id(&tx, id).await {
            Some(c) => c,
            None => return oops(StatusCode::NOT_FOUND, "no such ticket"),
        };
        let row = tx
            .query_opt(
                "select title, body from tickets where id = $1 and deleted_at is null",
                &[&canonical],
            )
            .await;
        let Ok(Some(r)) = row else {
            return oops(StatusCode::NOT_FOUND, "no such ticket");
        };
        // Берём ТЕКУЩИЙ текст, а не вектор из индекса: индекс мог отстать, и
        // тогда искали бы похожих на прошлую редакцию тикета.
        let t: String = r.get(0);
        let b: String = r.get(1);
        tx.commit().await.ok();
        (t, b, Some(canonical))
    } else {
        let tx = match db::begin(&mut client, &ws).await {
            Ok(t) => t,
            Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };
        let enabled = match vector_enabled(&tx).await {
            Ok(e) => e,
            Err(e) => {
                tracing::error!(error = %e, workspace = %ws, "vector policy did not read");
                return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
            }
        };
        policy_min = vector_stop_policy(&tx).await.map(|(_, m)| m).unwrap_or(policy_min);
        tx.commit().await.ok();
        if !enabled {
            return oops(
                StatusCode::CONFLICT,
                "vectorisation is switched off in this workspace: there is nothing to search",
            );
        }
        let t = p.text.unwrap_or_default();
        let b = p.body.unwrap_or_default();
        if t.trim().is_empty() && b.trim().is_empty() {
            return oops(StatusCode::BAD_REQUEST, "give text or a ticket id");
        }
        (t, b, None)
    };
    drop(client);

    let limit = p.limit.unwrap_or(crate::vector::SIMILAR_LIMIT as i64).clamp(1, SIMILAR_MAX as i64) as usize;
    // The default comes from the workspace policy, not from the binary: a
    // threshold tuned for one corpus has no business governing another.
    let min = p.min_score.unwrap_or(policy_min).clamp(0.0, 1.0);
    // Просим на один больше, когда ищем похожих на тикет: сам он найдётся
    // первым с оценкой 1.0, и без запаса выдача была бы короче заказанной.
    let ask = if exclude.is_some() { limit + 1 } else { limit };

    // `all: true` — по умолчанию ищем у ВСЕХ, а не только своё: дубль заводят
    // поверх чужого тикета чаще, чем поверх своего.
    let filters = crate::filter::Filters {
        status: p.status.clone(),
        tag: p.tag.clone(),
        strict: p.strict,
        title: None,
        assignee: p.assignee.clone(),
        project: p.project.clone(),
        module: p.module.clone(),
        all: p.assignee.is_none(),
    };
    let bound = filters.bind("");
    let narrowed = p.status.is_some() || p.tag.is_some() || p.assignee.is_some()
        || p.project.is_some() || p.module.is_some();

    match crate::vector::similar(
        &v, &app.pool, &ws, &text, &body, ask, min,
        narrowed.then_some(&bound),
    )
    .await
    {
        Ok(hits) => {
            let out: Vec<_> = hits
                .into_iter()
                .filter(|h| Some(&h.id) != exclude.as_ref())
                .take(limit)
                .collect();
            (StatusCode::OK, Json(json!({ "similar": out }))).into_response()
        }
        Err(e) => {
            tracing::warn!(error = %e, workspace = %ws, "поиск похожих не удался");
            oops(StatusCode::BAD_GATEWAY, "the similarity search is unavailable right now")
        }
    }
}

pub async fn create(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Json(p): Json<Create>,
) -> Response {
    let (mut client, actor, ws) = match enter(&app, &headers, p.workspace.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    if p.title.trim().is_empty() {
        return oops(StatusCode::BAD_REQUEST, "a ticket must have a title");
    }

    // Поиск похожих ДО заведения — и до открытия транзакции: обращение к
    // провайдеру занимает секунды, а транзакция, открытая на это время, держала
    // бы соединение и блокировки впустую.
    let searching = if p.skip_search { None } else { app.vector.clone() };
    if let Some(v) = searching {
            let (stopping, min_score) = {
                match db::begin(&mut client, &ws).await {
                    Ok(tx) => {
                        let on = vector_enabled(&tx).await;
                        let pol = vector_stop_policy(&tx).await;
                        tx.commit().await.ok();
                        match (on, pol) {
                            (Ok(true), Ok((stop, score))) => (stop, score),
                            (Ok(_), Ok(_)) => (false, crate::vector::NEAR_DUPLICATE),
                            (Err(err), _) | (_, Err(err)) => {
                                tracing::error!(error = %err, workspace = %ws, "vector policy did not read");
                                (false, crate::vector::NEAR_DUPLICATE)
                            }
                        }
                    }
                    Err(_) => (false, crate::vector::NEAR_DUPLICATE),
                }
            };
            if stopping {
                let body = p.body.clone().unwrap_or_default();
                match crate::vector::similar(
                    &v,
                    &app.pool,
                    &ws,
                    &p.title,
                    &body,
                    crate::vector::SIMILAR_LIMIT,
                    min_score,
                    // На заведении отбора нет намеренно: дубль закрытого тикета
                    // — самое ценное, что здесь можно сказать.
                    None,
                )
                .await
                {
                    Ok(hits) if !hits.is_empty() => {
                        // Верхняя оценка в журнал: порог выбран на тридцати
                        // тикетах, и вторую его итерацию надо считать по
                        // накопленному на живом объёме, а не по новому предположению.
                        tracing::info!(
                            workspace = %ws,
                            found = hits.len(),
                            top = hits[0].score,
                            top_ticket = %hits[0].id,
                            "заведение остановлено: похожие уже есть"
                        );
                        return (
                            StatusCode::CONFLICT,
                            Json(json!({
                                "error": "Look at these first. Same work — edit that ticket. Not the same — send skip_search: true.",
                                "similar": hits
                            })),
                        )
                            .into_response();
                    }
                    Ok(_) => {}
                    // Провайдер лёг — заводим. Иначе его отказ останавливал бы
                    // всю работу команды, а цена ошибки здесь несравнима:
                    // пропущенный дубль правится, ненаведённый тикет теряется.
                    Err(e) => tracing::warn!(
                        error = %e,
                        workspace = %ws,
                        "похожих не искал, завожу как есть"
                    ),
                }
            }
        }

    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    };

    // Пределы длины — на ЗАПИСИ, единым местом для всех клиентов: CLI, локальный
    // MCP и удалённый ходят одной ручкой, и проверка в каждом из них разошлась
    // бы, как уже расходились схемы.
    if let Some(m) = check_len(&tx, "title", &p.title, None).await {
        return oops(StatusCode::BAD_REQUEST, &m);
    }
    if let Some(b) = &p.body {
        if let Some(m) = check_len(&tx, "body", b, None).await {
            return oops(StatusCode::BAD_REQUEST, &m);
        }
    }

    let status = p.status.clone().unwrap_or_else(|| "open".into());
    if let Some(m) = p.module.as_deref() {
        if module_is_archived(&tx, p.project.as_deref(), m).await {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("module {m} is archived — it cannot be chosen for new work")})),
            )
                .into_response();
        }
    }
    // Проект заводится по ходу: справочник наполняется теми проектами, которые
    // реально встречаются, а не отдельным обрядом заведения.
    if let Some(proj) = p.project.as_deref() {
        if tx
            .execute("insert into projects (id, name) values ($1, $1) on conflict (id) do nothing", &[&proj])
            .await
            .is_err()
        {
            return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
        }
    }

    // Хвост случайный, а уникальность держит первичный ключ. Столкнулись —
    // берём новый и пробуем снова: проверять занятость чтением значит вернуть
    // ту самую гонку, из-за которой мы уехали с Notion.
    // Префикс — имя проекта, а без него имя воркспейса: идентификатор обязан
    // быть узнаваемым на глаз, это его главное свойство.
    let prefix = p.project.clone().unwrap_or_else(|| ws.clone());
    let mut id = p.id.clone().unwrap_or_else(|| format!("{prefix}-{}", new_tail()));
    let mut inserted = Ok(0);
    for attempt in 0..5 {
        if attempt > 0 {
            // Явный идентификатор от импорта не переизобретаем: занят — значит
            // такой тикет уже есть, и это ответ, а не повод придумать другой.
            if p.id.is_some() {
                break;
            }
            id = format!("{prefix}-{}", new_tail());
        }
        inserted = tx
            .execute(
                "insert into tickets (id, title, status, priority, type, assignee, project_id, tags, body, module)
                 values ($1,$2,$3,$4,$5,$6,$7,$8,coalesce($9,''),$10)
                 on conflict (id) do nothing",
                &[&id, &p.title, &status, &p.priority, &p.kind, &p.assignee, &p.project, &p.tags, &p.body, &p.module],
            )
            .await;
        if !matches!(inserted, Ok(0)) {
            break;
        }
    }

    match inserted {
        Ok(0) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error": "that identifier is taken", "id": id})),
            )
                .into_response()
        }
        Ok(_) => {}
        Err(e) => {
            // Ссылка на несуществующий статус, приоритет или исполнителя — это
            // ошибка вызывающего, а не сбой: сказать, что именно не так,
            // дешевле, чем «внутренняя ошибка» и чтение логов.
            let db = e.as_db_error();
            let constraint = db.and_then(|d| d.constraint()).unwrap_or_default();
            // В журнал идут подробности, а не «db error»: без них причину
            // приходится искать перебором даже по логу.
            tracing::error!(
                error = %e,
                constraint = %constraint,
                detail = %db.map(|d| d.message()).unwrap_or_default(),
                "не удалось создать тикет"
            );
            let msg = constraint.to_string();
            // База знает, КАКОЕ ограничение не прошло, — значит и человек должен
            // знать. Прежнее сообщение перечисляло пять возможных причин разом:
            // «проверьте статус, приоритет, исполнителя, модуль и формат id».
            // По такому тексту виновника ищут перебором, и именно этим занялся
            // PM, когда заведение отказало.
            if let Some(m) = explain_constraint(&msg) {
                return oops(StatusCode::BAD_REQUEST, &m);
            }
            // Ограничение нарушено, но какое — не опознали: это всё равно
            // ошибка запроса, а не сбой.
            let code = if db.is_some() {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            return oops(code, "could not create the ticket: check the status, priority, assignee, module and the id format");
        }
    }

    // Зависимости ставятся после самой строки и только на существующие цели:
    // ребро в никуда молча превратило бы «жду тикет X» в «ничего не жду».
    let mut missing: Vec<String> = Vec::new();
    for dep in &p.deps {
        match tx
            .execute(
                "insert into deps (ticket_id, depends_on)
                 select $1, id from tickets where id = $2 and deleted_at is null
                 on conflict do nothing",
                &[&id, dep],
            )
            .await
        {
            Ok(0) => missing.push(dep.clone()),
            Ok(_) => {}
            Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        }
    }
    if !missing.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("no such tickets: {}", missing.join(", "))})),
        )
            .into_response();
    }

    // Долг ставится ДО commit, в той же транзакции: если процесс умрёт между
    // ними, тикет не окажется записанным без долга.
    if let Err(e) = enqueue_upsert(&tx, &id).await {
        tracing::error!(error = %e, ticket = %id, workspace = %ws, "долг индексации не поставлен");
        return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
    }
    if tx.commit().await.is_err() {
        return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
    }
    tracing::info!(actor = %actor.user_id, ticket = %id, workspace = %ws, "тикет создан");
    // Будим слив ПОСЛЕ коммита: до него долга ещё нет, и будить нечего.
    crate::vector::wake(&app);
    (StatusCode::CREATED, Json(json!({"id": id, "status": status}))).into_response()
}

/// Дерево зависимостей: на чём стоит тикет и что стоит на нём.
///
/// Считается в базе рекурсивным запросом, а не обходом по одному запросу на
/// узел: в acme 685 рёбер, и обход из клиента упёрся бы в то же, из-за
/// чего мы ушли из Notion. `cycle` в SQL нужен не для красоты — граф
/// зависимостей никто не проверяет на ацикличность, и один цикл повесил бы
/// запрос навсегда.
pub async fn deps(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<Ws>,
) -> Response {
    let (mut client, _actor, ws) = match enter(&app, &headers, q.workspace.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    };

    let Ok(Some(root)) = tx
        .query_opt("select id, title, status from tickets where lower(id) = lower($1) and deleted_at is null", &[&id])
        .await
    else {
        return oops(StatusCode::NOT_FOUND, "no such ticket");
    };
    let root_id: String = root.get(0);

    let walk = |direction: &'static str| {
        let from = if direction == "up" { "depends_on" } else { "ticket_id" };
        let to = if direction == "up" { "ticket_id" } else { "depends_on" };
        format!(
            "with recursive walk(id, depth) as (
                 select d.{from}, 1 from deps d where d.{to} = $1
                 union all
                 select d.{from}, w.depth + 1
                   from deps d join walk w on d.{to} = w.id
                  where w.depth < 20
             ) cycle id set looped using path
             select distinct on (w.id) w.id, t.title, t.status, w.depth,
                    t.deleted_at is not null as removed
               from walk w join tickets t on t.id = w.id
              order by w.id, w.depth"
        )
    };

    let mut out = serde_json::Map::new();
    out.insert("id".into(), json!(root_id));
    out.insert("title".into(), json!(root.get::<_, String>(1)));
    out.insert("status".into(), json!(root.get::<_, String>(2)));

    for dir in ["up", "down"] {
        match tx.query(&walk(dir), &[&root_id]).await {
            Ok(rows) => {
                let items: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|r| {
                        json!({
                            "id": r.get::<_, String>(0),
                            "title": r.get::<_, String>(1),
                            "status": r.get::<_, String>(2),
                            "depth": r.get::<_, i32>(3),
                            // Удалённый тикет остаётся в дереве: связь никуда
                            // не делась. Но в списках его нет, и без пометки
                            // читатель не поймёт, почему не находит его.
                            "removed": r.get::<_, bool>(4),
                        })
                    })
                    .collect();
                out.insert(dir.into(), json!(items));
            }
            Err(e) => {
                tracing::error!(error = %e, "не удалось обойти зависимости");
                return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
            }
        }
    }
    Json(serde_json::Value::Object(out)).into_response()
}

/// Удаление тикета — отметкой, а не delete.
///
/// Настоящее удаление унесло бы рёбра зависимостей: у deps стоит on delete
/// cascade, поэтому «удалить один тикет» тихо означало бы «снять зависимость
/// у всех, кто его ждал», и они стали бы готовыми к работе без объяснения.
/// Отметка сохраняет и связи, и возможность вернуть — в Notion это была
/// корзина, и люди на неё полагались.
pub async fn remove(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<Ws>,
) -> Response {
    let (mut client, actor, ws) = match enter(&app, &headers, q.workspace.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    };
    // Идентификатор приводится к каноническому виду сразу после входа в
    // воркспейс: дальше все сравнения точные, и ни одно из них не надо помнить.
    let id = match canonical_id(&tx, &id).await {
        Some(v) => v,
        None => return oops(StatusCode::NOT_FOUND, "no such ticket"),
    };


    // Кто на нём стоит — говорится вслух до удаления, а не выясняется потом.
    let waiting: Vec<String> = match tx
        .query(
            "select d.ticket_id from deps d
               join tickets t on t.id = d.ticket_id and t.deleted_at is null
              where d.depends_on = $1",
            &[&id],
        )
        .await
    {
        Ok(rows) => rows.iter().map(|r| r.get::<_, String>(0)).collect(),
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    };

    let affected = tx
        .execute(
            "update tickets set deleted_at = now() where lower(id) = lower($1) and deleted_at is null",
            &[&id],
        )
        .await;
    // Удаление точки из индекса — тоже долг, и тоже в этой транзакции.
    // `done` сюда не попадает: закрытый тикет остаётся живым и должен находиться.
    if matches!(affected, Ok(n) if n > 0) {
        if let Err(e) = enqueue_delete(&tx, &id).await {
            tracing::error!(error = %e, ticket = %id, workspace = %ws, "долг удаления не поставлен");
            return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
        }
    }
    match affected {
        Ok(0) => oops(StatusCode::NOT_FOUND, "no such ticket, or it is already removed"),
        Ok(_) if tx.commit().await.is_ok() => {
            tracing::info!(actor = %actor.user_id, ticket = %id, workspace = %ws, "тикет удалён");
            crate::vector::wake(&app);
            Json(json!({"id": id, "deleted": true, "still_waiting_on_it": waiting})).into_response()
        }
        _ => oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    }
}

/// Содержимое воркспейса одним ответом: статусы с группами, приоритеты,
/// проекты, люди.
///
/// Четыре прежние команды — schema, users, projects, workspaces — отвечали на
/// один вопрос «что здесь есть», отличаясь только куском ответа.
pub async fn meta(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Query(q): Query<Ws>,
) -> Response {
    let (mut client, actor, ws) = match enter(&app, &headers, q.workspace.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };

    // Люди читаются ДО транзакции, и это не стиль, а необходимость.
    //
    // db::begin делает `SET LOCAL ROLE ntk_ws_<воркспейс>`, а ntk_api объявлен
    // NOINHERIT: внутри транзакции прав на схему core уже нет, и запрос к
    // core.users отвечает «permission denied for schema core». Изоляция
    // работает как задумано — общее читается до входа в роль воркспейса.
    //
    // Раньше запрос стоял внутри и отказ ГЛОТАЛСЯ через unwrap_or_default:
    // meta отвечала «людей нет» для ЛЮБОГО воркспейса, хотя в core.user_workspaces
    // их было пять, шесть и два. Пустой список неотличим от «никого не завели»,
    // поэтому агенты не могли узнать допустимые имена исполнителей и подставляли
    // выдуманные, а внешний ключ на core.users отвергал запись уже потом.
    let people: Vec<serde_json::Value> = match client
        .query(
            "select u.id, u.kind from core.users u
               join core.user_workspaces uw on uw.user_id = u.id
              where uw.workspace = $1 and u.active order by u.id",
            &[&ws],
        )
        .await
    {
        Ok(rows) => rows
            .iter()
            .map(|r| json!({"id": r.get::<_, String>(0), "kind": r.get::<_, String>(1)}))
            .collect(),
        Err(e) => {
            // Отказ называется вслух: пустой список означал бы «никого нет»,
            // а это другой ответ.
            tracing::error!(error = %e, workspace = %ws, "не удалось прочитать людей воркспейса");
            return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
        }
    };

    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    };

    let limits = tx
        .query("select field, max_len from write_limits order by field", &[])
        .await
        .map(|rows| {
            rows.iter()
                .map(|r| (r.get::<_, String>(0), r.get::<_, i32>(1)))
                .collect::<std::collections::BTreeMap<_, _>>()
        })
        .unwrap_or_default();

    let statuses = tx
        .query("select s.name, s.grp, p.requires_force from statuses s
                  join status_policy p on p.grp = s.grp order by s.sort", &[])
        .await
        .map(|rows| {
            rows.iter()
                .map(|r| json!({
                    "name": r.get::<_, String>(0),
                    "group": r.get::<_, String>(1),
                    // Гард живёт в данных: правка тикета вне группы todo
                    // требует force, и это видно отсюда, а не из чтения кода.
                    "requires_force": r.get::<_, bool>(2),
                }))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let priorities = tx
        .query("select name from priorities order by rank", &[])
        .await
        .map(|rows| rows.iter().map(|r| r.get::<_, String>(0)).collect::<Vec<_>>())
        .unwrap_or_default();

    let projects = tx
        .query("select id from projects where not archived order by id", &[])
        .await
        .map(|rows| rows.iter().map(|r| r.get::<_, String>(0)).collect::<Vec<_>>())
        .unwrap_or_default();

    // Модули перечисляются рядом с проектами: агенту неоткуда узнать
    // допустимые значения, а угадывать он не должен.
    let modules = tx
        .query("select project_id, name, archived_at is not null from modules order by project_id, name", &[])
        .await
        .map(|rows| {
            rows.iter()
                .map(|r| json!({
                    "project": r.get::<_, String>(0),
                    "name": r.get::<_, String>(1),
                    "archived": r.get::<_, bool>(2),
                }))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    Json(json!({
        "workspace": ws,
        "workspaces": actor.workspaces,
        "statuses": statuses,
        "priorities": priorities,
        "projects": projects,
        "modules": modules,
        "people": people,
        // Пределы отдаются наружу, чтобы число было ОДНО. Клиенту нужно знать
        // порог до отправки, иначе он или гоняет впустую большие тела, или
        // держит свою копию числа — а копия разойдётся в первый же раз, когда
        // владелец подвинет порог в таблице.
        "limits": limits,
    }))
    .into_response()
}


/// Архивный модуль нельзя выбрать заново.
///
/// Внешний ключ этого не различает: строка есть, ссылка валидна — именно
/// поэтому существующие тикеты продолжают жить и менять статус. Запрет
/// касается только НОВОГО выбора, и проверяется он здесь, в той же
/// транзакции, что и запись.
async fn module_is_archived(
    tx: &deadpool_postgres::Transaction<'_>,
    project: Option<&str>,
    module: &str,
) -> bool {
    let Some(project) = project else { return false };
    matches!(
        tx.query_opt(
            "select 1 from modules where project_id = $1 and name = $2 and archived_at is not null",
            &[&project, &module],
        )
        .await,
        Ok(Some(_))
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Modules {
    workspace: Option<String>,
    /// Полный список модулей проекта. Именно полный: это ЗАМЕНА набора, а не
    /// добавление. Иначе удалённый модуль остался бы в реестре навсегда, и
    /// реестр разошёлся бы с тем, что есть на самом деле.
    modules: Vec<String>,
}

/// Замена набора модулей проекта целиком.
///
/// NTK не выясняет, что считать модулем: список приходит готовым от того, кто
/// владеет его источником. Здесь только хранение и проверка.
///
/// Исчезнувший модуль, на который ссылаются тикеты, отвергает ВЕСЬ запрос.
/// Удалить его молча значило бы либо оборвать ссылки, либо оставить тикеты
/// указывающими в пустоту — и то и другое обнаружилось бы позже и не тем, кто
/// это сделал.
pub async fn set_modules(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(project): Path<String>,
    Json(p): Json<Modules>,
) -> Response {
    let (mut client, actor, ws) = match enter(&app, &headers, p.workspace.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    };

    if tx
        .query_opt("select 1 from projects where id = $1", &[&project])
        .await
        .ok()
        .flatten()
        .is_none()
    {
        return oops(StatusCode::NOT_FOUND, "no such project");
    }

    let wanted: Vec<String> = {
        let mut seen = std::collections::BTreeSet::new();
        p.modules
            .iter()
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty() && seen.insert(m.clone()))
            .collect()
    };

    let existing: Vec<String> = match tx
        .query("select name from modules where project_id = $1", &[&project])
        .await
    {
        Ok(rows) => rows.iter().map(|r| r.get::<_, String>(0)).collect(),
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    };

    let removed: Vec<&String> = existing.iter().filter(|e| !wanted.contains(e)).collect();
    let added: Vec<&String> = wanted.iter().filter(|w| !existing.contains(w)).collect();

    // Исчезнувший модуль не отвергает замену и не пропадает молча: если на
    // него ссылаются, он уходит в архив — ссылка остаётся рабочей, а выбрать
    // его для новой работы больше нельзя. Если не ссылается никто, он просто
    // удаляется: хранить след того, чего никто не помнит, незачем.
    let mut archived: Vec<String> = Vec::new();
    let mut deleted: Vec<String> = Vec::new();
    for m in &removed {
        let used: i64 = match tx
            .query_one(
                "select count(*) from tickets where project_id = $1 and module = $2",
                &[&project, m],
            )
            .await
        {
            Ok(r) => r.get(0),
            Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };
        // Удалённые тикеты тоже считаются: их можно вернуть, и вернувшийся
        // тикет со ссылкой в пустоту — это та же потеря, только отложенная.
        if used > 0 {
            // Считаем строки, а не факт успеха: модуль, который был архивным
            // и до этой замены, в отчёт попасть не должен — иначе каждая
            // замена докладывала бы об уборке, которой не было.
            match tx
                .execute(
                    "update modules set archived_at = now()
                      where project_id = $1 and name = $2 and archived_at is null",
                    &[&project, m],
                )
                .await
            {
                Ok(n) if n > 0 => archived.push((*m).clone()),
                Ok(_) => {}
                Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
            }
        } else {
            if tx.execute("delete from modules where project_id = $1 and name = $2", &[&project, m]).await.is_err() {
                return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
            }
            deleted.push((*m).clone());
        }
    }

    // Вернувшийся в список модуль снова действующий. Через insert его не
    // провести: строка существует, и `on conflict do nothing` промолчал бы,
    // оставив архивным то, что источник считает живым.
    let mut restored: Vec<String> = Vec::new();
    for m in &wanted {
        let back = tx
            .execute(
                "update modules set archived_at = null
                  where project_id = $1 and name = $2 and archived_at is not null",
                &[&project, m],
            )
            .await;
        match back {
            Ok(n) if n > 0 => restored.push(m.clone()),
            Ok(_) => {}
            Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        }
    }

    for m in &added {
        if tx
            .execute("insert into modules (project_id, name) values ($1,$2) on conflict do nothing", &[&project, m])
            .await
            .is_err()
        {
            return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
        }
    }

    if tx.commit().await.is_err() {
        return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
    }
    tracing::info!(actor = %actor.user_id, project = %project, workspace = %ws,
                   added = added.len(), archived = archived.len(), deleted = deleted.len(),
                   restored = restored.len(), "module registry replaced");
    Json(json!({
        "project": project,
        "total": wanted.len(),
        "added": added,
        "archived": archived,
        "deleted": deleted,
        "restored": restored,
    }))
    .into_response()
}

// ── Жизненный цикл проекта ───────────────────────────────────────────────────
//
// Проекты до сих пор только заводились: строка появлялась сама при первом
// тикете и не исчезала никогда. Переименования нет и не будет — id проекта
// сидит префиксом в идентификаторах тикетов, а те первичные ключи. Поэтому
// «переименовать» здесь значит перенести тикеты и убрать опустевшее имя из
// выбора, и обе половины должны существовать: без второй в `ntk meta` навсегда
// остаётся алиас, в который можно записать по недосмотру.

/// Модули, которых нет в целевом проекте.
///
/// Внешний ключ тикета идёт на ПАРУ (project_id, module), поэтому перенос
/// тикета с модулем туда, где такого имени не заведено, отвергнет база — и
/// человек увидит «внутреннюю ошибку» вместо списка того, что надо создать.
/// Считаем заранее и называем поимённо.
///
/// Архивные не отсеиваем намеренно: архив запрещает ВЫБИРАТЬ модуль для новой
/// работы, а здесь ссылка уже существует и обязана остаться рабочей — ровно то
/// разделение, ради которого заводился archived_at (sql/017).
pub(crate) fn modules_missing_in_target(source: &[String], target: &[String]) -> Vec<String> {
    absent_from(source, target)
}

/// Имена, похожие на промах настолько, что их стоит предложить.
///
/// Подстроки одной опечатки не ловят: `bakend` не содержит `backend` и не
/// содержится в нём, а имелся в виду именно он. Поэтому расстояние
/// редактирования, а не вхождение — пропущенная, лишняя или переставленная
/// буква остаётся в пределах двух правок.
pub(crate) fn near_misses(asked: &str, known: &[String]) -> Vec<String> {
    let mut near: Vec<(usize, String)> = known
        .iter()
        .filter_map(|k| {
            if k.contains(asked) || asked.contains(k.as_str()) {
                return Some((0, k.clone()));
            }
            // Порог зависит от длины: для `skk` расстояние 2 накрывает
            // половину коротких хэндлов (kf, kh, s4, sh), и подсказка из шума
            // хуже, чем её отсутствие. Одна правка на короткое имя, две — на
            // длинное, где опечатка чаще двойная.
            let d = edit_distance(asked, k);
            let limit = if asked.chars().count() <= 4 { 1 } else { 2 };
            (d <= limit).then_some((d, k.clone()))
        })
        .collect();
    near.sort();
    near.into_iter().take(5).map(|(_, k)| k).collect()
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Какие из названных имён отсутствуют среди известных.
///
/// Общая для модулей, статусов и проектов: вопрос «что из спрошенного мы не
/// знаем» один и тот же, а ответ на него всюду обязан быть списком имён, а не
/// пустой выдачей. Пустая выдача — тот же ноль, что и «ничего не подошло», и
/// отличить опечатку от ответа по ней нельзя.
pub(crate) fn absent_from(asked: &[String], known: &[String]) -> Vec<String> {
    let have: std::collections::BTreeSet<&str> = known.iter().map(String::as_str).collect();
    let mut miss: Vec<String> = asked
        .iter()
        .filter(|m| !have.contains(m.as_str()))
        .cloned()
        .collect();
    miss.sort();
    miss.dedup();
    miss
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectPatch {
    workspace: Option<String>,
    /// Убрать из выбора или вернуть в него.
    archived: bool,
}

/// Убрать опустевший проект из выбора, не стирая его.
///
/// Стереть нельзя: на id проекта ссылаются идентификаторы уже заведённых
/// тикетов, и удаление строки сделало бы их сиротами. Архив отвечает на другой
/// вопрос — «можно ли выбрать его для новой работы», — и отвечает «нет».
///
/// Архивировать непустой проект отказываемся: тикеты никуда не денутся, но
/// пропадут из `meta`, и работа станет невидимой, оставшись живой.
pub async fn patch_project(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(project): Path<String>,
    Json(p): Json<ProjectPatch>,
) -> Response {
    let (mut client, _actor, ws) = match enter(&app, &headers, p.workspace.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    };

    if tx
        .query_opt("select 1 from projects where id = $1", &[&project])
        .await
        .ok()
        .flatten()
        .is_none()
    {
        return oops(StatusCode::NOT_FOUND, "no such project");
    }

    if p.archived {
        let left: i64 = match tx
            .query_one("select count(*) from tickets where project_id = $1", &[&project])
            .await
        {
            Ok(r) => r.get(0),
            Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };
        if left > 0 {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": format!(
                        "project {project} still holds {left} tickets: move them, or the work stays alive while becoming invisible"
                    ),
                    "tickets": left
                })),
            )
                .into_response();
        }
    }

    if tx
        .execute("update projects set archived = $2 where id = $1", &[&project, &p.archived])
        .await
        .is_err()
    {
        return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
    }
    if tx.commit().await.is_err() {
        return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
    }
    Json(json!({ "project": project, "archived": p.archived })).into_response()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectMove {
    workspace: Option<String>,
    /// Куда переносим. Заводится, если ещё нет, — как и при заведении тикета.
    to: String,
}

/// Перенести ВСЕ тикеты проекта в другой проект, одной транзакцией.
///
/// Поштучный `PATCH` тоже умеет менять проект, но на двух тысячах тикетов это
/// две тысячи запросов, каждый со своим окном на отказ посередине. Здесь либо
/// переезжают все, либо никто.
///
/// Идентификаторы тикетов НЕ меняются: они первичные ключи, на них ссылаются
/// зависимости и вся переписка снаружи. Поэтому после переноса префикс id
/// перестаёт совпадать с именем проекта — это цена сохранности ссылок, и она
/// выбрана осознанно.
pub async fn move_project(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(from): Path<String>,
    Json(p): Json<ProjectMove>,
) -> Response {
    let (mut client, _actor, ws) = match enter(&app, &headers, p.workspace.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let to = p.to.trim().to_string();
    if to.is_empty() {
        return oops(StatusCode::BAD_REQUEST, "name the target project");
    }
    if to == from {
        return oops(StatusCode::BAD_REQUEST, "moving into the same project means nothing");
    }
    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    };

    if tx
        .query_opt("select 1 from projects where id = $1", &[&from])
        .await
        .ok()
        .flatten()
        .is_none()
    {
        return oops(StatusCode::NOT_FOUND, "no such project");
    }
    if tx
        .execute(
            "insert into projects (id, name) values ($1, $1) on conflict (id) do nothing",
            &[&to],
        )
        .await
        .is_err()
    {
        return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
    }

    // Модули считаем ДО записи: иначе внешний ключ на пару отвергнет перенос
    // где-то посередине, и вместо списка недостающих имён человек получит
    // «внутреннюю ошибку».
    let source: Vec<String> = match tx
        .query(
            "select distinct module from tickets where project_id = $1 and module is not null",
            &[&from],
        )
        .await
    {
        Ok(rows) => rows.iter().map(|r| r.get::<_, String>(0)).collect(),
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    };
    let target: Vec<String> = match tx
        .query("select name from modules where project_id = $1", &[&to])
        .await
    {
        Ok(rows) => rows.iter().map(|r| r.get::<_, String>(0)).collect(),
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    };
    let missing = modules_missing_in_target(&source, &target);
    if !missing.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!(
                    "project {to} has no modules matching the ones the moved tickets reference: {}",
                    missing.join(", ")
                ),
                "missing_modules": missing
            })),
        )
            .into_response();
    }

    let moved = match tx
        .execute("update tickets set project_id = $2 where project_id = $1", &[&from, &to])
        .await
    {
        Ok(n) => n,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    };
    if tx.commit().await.is_err() {
        return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
    }
    Json(json!({ "from": from, "to": to, "moved": moved })).into_response()
}

#[cfg(test)]
mod project_lifecycle_tests {
    use super::modules_missing_in_target;

    #[test]
    fn names_every_module_the_target_lacks() {
        let miss = modules_missing_in_target(
            &["db".into(), "api".into(), "web".into()],
            &["api".into()],
        );
        assert_eq!(miss, vec!["db".to_string(), "web".to_string()]);
    }

    #[test]
    fn nothing_missing_means_the_move_is_allowed() {
        let miss =
            modules_missing_in_target(&["db".into()], &["db".into(), "api".into()]);
        assert!(miss.is_empty(), "лишний отказ: {miss:?}");
    }

    // Имя модуля уникально лишь ВНУТРИ проекта, поэтому совпадение проверяется
    // по имени в целевом реестре, а не по факту существования где-нибудь.
    #[test]
    fn tickets_without_modules_never_block_the_move() {
        assert!(modules_missing_in_target(&[], &[]).is_empty());
    }

    // Тот же вопрос для статусов и проектов: опечатка обязана назваться, а не
    // раствориться в пустой выдаче.
    // Ради этого случая проверка и заводилась: опечатка в одну букву.
    #[test]
    fn a_one_letter_typo_finds_its_target() {
        let near = super::near_misses("bakend", &["backend".into(), "web".into(), "ntk".into()]);
        assert!(near.contains(&"backend".to_string()), "не предложен backend: {near:?}");
    }

    // Короткие хэндлы: две правки превращают в «похожее» почти всё.
    #[test]
    fn short_handles_suggest_only_one_edit_away() {
        let near = super::near_misses(
            "skk",
            &["sk".into(), "kf".into(), "kh".into(), "s4".into(), "sh".into()],
        );
        assert_eq!(near, vec!["sk".to_string()], "подсказка из шума: {near:?}");
    }

    #[test]
    fn a_name_unlike_anything_gets_no_suggestions() {
        let near = super::near_misses("zzzzzzzz", &["backend".into(), "ntk".into()]);
        assert!(near.is_empty(), "выдуманы похожие: {near:?}");
    }

    #[test]
    fn a_mistyped_status_is_named_not_swallowed() {
        let unknown = super::absent_from(
            &["open".into(), "in_progres".into()],
            &["open".into(), "in_progress".into(), "done".into()],
        );
        assert_eq!(unknown, vec!["in_progres".to_string()]);
    }

    #[test]
    fn a_list_of_known_statuses_passes_whole() {
        let unknown = super::absent_from(
            &["open".into(), "blocked".into()],
            &["open".into(), "blocked".into(), "done".into()],
        );
        assert!(unknown.is_empty(), "исправный список отвергнут: {unknown:?}");
    }

    #[test]
    fn one_missing_module_is_reported_once() {
        let miss = modules_missing_in_target(&["db".into(), "db".into()], &[]);
        assert_eq!(miss, vec!["db".to_string()], "дубли в отказе только мешают читать");
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModulesAdd {
    workspace: Option<String>,
    /// Имена, которые надо завести. Список ДОПОЛНЯЕТ реестр и ничего из него
    /// не убирает.
    add: Vec<String>,
}

/// Завести модули, не трогая остальной реестр.
///
/// Замена набора существовала с самого начала, добавления не было — и это
/// заставляло присылать ПОЛНЫЙ список ради трёх новых имён. Список из восьмидесяти
/// строк, отправленный ради трёх, — это семьдесят семь возможностей молча
/// потерять строку: любая забытая уходит из действующих. Ревью, отвергающее
/// такую операцию, право, и обходить его через «пришлите весь список
/// аккуратно» неправильно: аккуратность не свойство операции.
///
/// Имя, которое уже есть и действует, — не ошибка, а ничего: повторный вызов
/// с тем же списком обязан быть безопасным.
///
/// Архивное имя возвращается в действующие. Просьба завести модуль означает,
/// что работа по нему снова идёт; отказать здесь значило бы требовать полной
/// замены ровно в том случае, ради которого добавление и заводилось.
pub async fn add_modules(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(project): Path<String>,
    Json(p): Json<ModulesAdd>,
) -> Response {
    let (mut client, _actor, ws) = match enter(&app, &headers, p.workspace.as_deref()).await {
        Ok(v) => v,
        Err(r) => return r,
    };
    let wanted: Vec<String> = {
        let mut seen = std::collections::BTreeSet::new();
        p.add
            .iter()
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty() && seen.insert(m.clone()))
            .collect()
    };
    if wanted.is_empty() {
        return oops(StatusCode::BAD_REQUEST, "name at least one module");
    }

    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
    };
    if tx
        .query_opt("select 1 from projects where id = $1", &[&project])
        .await
        .ok()
        .flatten()
        .is_none()
    {
        return oops(StatusCode::NOT_FOUND, "no such project");
    }

    let mut added: Vec<String> = Vec::new();
    let mut restored: Vec<String> = Vec::new();
    for m in &wanted {
        let was: Option<Option<std::time::SystemTime>> = match tx
            .query_opt(
                "select archived_at from modules where project_id = $1 and name = $2",
                &[&project, m],
            )
            .await
        {
            Ok(r) => r.map(|row| row.get(0)),
            Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };
        match was {
            // Действует — делать нечего.
            Some(None) => {}
            Some(Some(_)) => {
                if tx
                    .execute(
                        "update modules set archived_at = null
                          where project_id = $1 and name = $2",
                        &[&project, m],
                    )
                    .await
                    .is_err()
                {
                    return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
                }
                restored.push(m.clone());
            }
            None => {
                if tx
                    .execute(
                        "insert into modules (project_id, name) values ($1, $2)",
                        &[&project, m],
                    )
                    .await
                    .is_err()
                {
                    return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
                }
                added.push(m.clone());
            }
        }
    }
    if tx.commit().await.is_err() {
        return oops(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
    }
    // Ничего не удалено и не заархивировано — это свойство операции, и оно
    // названо в ответе, чтобы проверяющему не приходилось верить на слово.
    Json(json!({
        "project": project,
        "added": added,
        "restored": restored,
        "archived": Vec::<String>::new(),
        "deleted": Vec::<String>::new()
    }))
    .into_response()
}

#[cfg(test)]
mod create_wire_tests {
    use super::Create;

    // Заведение и правка обязаны звать одно и то же поле одинаково. Расхождение
    // стоило отказа разбора на КАЖДОМ создании с типом, а наружу оно выглядело
    // как «ответ сервиса не разобрался» — сообщение, из которого причина никак
    // не следует.
    #[test]
    fn create_accepts_type_as_the_wire_name() {
        let p: Create = serde_json::from_str(
            r#"{"workspace":"ws","title":"t","type":"feature"}"#,
        )
        .expect("type обязан приниматься");
        assert_eq!(p.kind.as_deref(), Some("feature"));
    }

    // Уже выпущенные клиенты шлют `kind`; одним переименованием их бы сломало.
    #[test]
    fn create_still_accepts_the_old_name() {
        let p: Create = serde_json::from_str(
            r#"{"workspace":"ws","title":"t","kind":"bug"}"#,
        )
        .expect("kind обязан приниматься как псевдоним");
        assert_eq!(p.kind.as_deref(), Some("bug"));
    }

    // deny_unknown_fields — намеренный выбор: опечатка в имени поля должна
    // отвергаться, а не проглатываться. Проверяем, что он на месте.
    #[test]
    fn an_unknown_field_is_still_refused() {
        let r = serde_json::from_str::<Create>(r#"{"workspace":"ws","title":"t","tipe":"x"}"#);
        assert!(r.is_err(), "неизвестное поле обязано отвергаться");
    }
}

/// Приводит идентификатор к тому виду, в котором он лежит в базе.
///
/// Идентификатор набирают руками и копируют из чужих сообщений, поэтому регистр
/// в нём случаен. `show` и `deps` искали через lower(id) и находили, а правка,
/// захват и вложения сравнивали точно и отвечали «такого тикета нет» — про
/// существующий тикет. Одно и то же имя означало разное в зависимости от ручки.
///
/// Приведение делается ОДИН раз на входе, а не правкой каждого сравнения:
/// сравнений десять, и следующее добавят снова точным. Индекс lower(id) есть,
/// поэтому это не стоит ничего.
pub(crate) async fn canonical_id(tx: &deadpool_postgres::Transaction<'_>, id: &str) -> Option<String> {
    tx.query_opt(
        "select id from tickets where lower(id) = lower($1) and deleted_at is null",
        &[&id],
    )
    .await
    .ok()
    .flatten()
    .map(|r| r.get(0))
}

/// Переводит нарушенное ограничение в понятное объяснение.
///
/// Принимает ИМЯ ограничения из структурированной ошибки, а не её текст.
/// Текстом пользоваться нельзя: `Error::to_string()` у драйвера возвращает
/// ровно «db error», без единой подробности. Прежняя проверка искала в этой
/// строке подстроку «foreign key» и потому не срабатывала никогда — нарушение
/// ограничения уходило наружу как 500, хотя ошибка была в запросе.
///
/// Возвращает None, если ограничение не опознано: выдумывать причину хуже,
/// чем признать её неизвестной.
pub(crate) fn explain_constraint(err: &str) -> Option<String> {
    let m = |field: &str, hint: &str| Some(format!("{field}. {hint}"));
    if err.contains("tickets_assignee_fkey") {
        return m("unknown assignee", "ntk meta lists the ones allowed.");
    }
    if err.contains("tickets_status_fkey") {
        return m("unknown status", "ntk meta lists the ones allowed.");
    }
    if err.contains("tickets_priority_fkey") {
        return m("unknown priority", "ntk meta lists the ones allowed.");
    }
    if err.contains("tickets_project_id_fkey") {
        return m("unknown project", "ntk meta gives the list.");
    }
    if err.contains("tickets_module_fk") {
        // Ключ идёт на ПАРУ, поэтому имя модуля из чужого проекта тоже не
        // подойдёт — сказать это сразу дешевле, чем искать опечатку в имени.
        return m(
            "that module is not registered in this project",
            "The foreign key is on the pair project+module, so a name from another project will not do either. ntk modules -P <project> lists the live ones; ntk modules -P <project> --add registers a new one.",
        );
    }
    if err.contains("tickets_id_check") {
        return m(
            "invalid identifier",
            "Letters, digits, hyphen and underscore are allowed, from 3 to 64 characters.",
        );
    }
    if err.contains("tickets_pkey") {
        return m("такой идентификатор уже занят", "The server assigns the identifier; send your own only when migrating data.");
    }
    None
}

#[cfg(test)]
mod explain_tests {
    use super::explain_constraint;

    // Сообщение обязано называть ОДНУ причину, а не перечислять пять: по списку
    // виновника ищут перебором.
    #[test]
    fn each_constraint_names_its_own_field() {
        for (err, want) in [
            ("... violates foreign key constraint \"tickets_assignee_fkey\"", "assignee"),
            ("... violates foreign key constraint \"tickets_module_fk\"", "module"),
            ("... violates check constraint \"tickets_id_check\"", "identifier"),
            ("... violates foreign key constraint \"tickets_status_fkey\"", "status"),
        ] {
            let got = explain_constraint(err).unwrap_or_default();
            assert!(got.contains(want), "for {err} expected a mention of {want}, got {got:?}");
        }
    }

    // Неопознанное не выдумывается: лучше прежнее общее сообщение, чем
    // уверенная неправда о причине.
    #[test]
    fn an_unknown_error_is_not_guessed() {
        assert!(explain_constraint("connection reset by peer").is_none());
    }
}

/// Появится ли цикл, если `ticket` начнёт ждать `target`.
///
/// Прямую самоссылку ловит сравнение идентификаторов, и её мало: A ждёт B, B
/// ждёт A — обе правки по отдельности законны, а вместе дают кольцо. Пришло
/// снаружи именно так: «тикет числится в собственных up и down». Прямой
/// самоссылки в базе и правда не было — до себя он доходил ЧЕРЕЗ цикл, и
/// выдача зависимостей честно его показывала.
///
/// Цена кольца не в некрасивой выдаче. Тикет в цикле не может быть
/// разблокирован никогда: каждый ждёт того, кто ждёт его. Очередь такой тикет
/// не выдаст, а почему — не скажет.
///
/// Идём ВВЕРХ от цели: если от неё по рёбрам «ждёт» достижим сам тикет, ребро
/// замкнёт кольцо. `cycle` в SQL обязателен — без него кольцо, уже стоящее в
/// базе, увело бы обход в бесконечность.
async fn would_cycle(
    tx: &deadpool_postgres::Transaction<'_>,
    ticket: &str,
    target: &str,
) -> bool {
    let row = tx
        .query_opt(
            "with recursive up as (
               select depends_on as id from deps where lower(ticket_id) = lower($2)
               union
               select d.depends_on from deps d join up on lower(d.ticket_id) = lower(up.id)
             )
             select 1 from up where lower(id) = lower($1) limit 1",
            &[&ticket, &target],
        )
        .await;
    matches!(row, Ok(Some(_)))
}

/// Пределы длины из таблицы `write_limits`.
///
/// Читаются на каждой записи, а не кешируются: таблица в две строки, запрос
/// дешевле, чем несогласованность после того, как владелец подвинул порог.
/// Порог для этого и лежит в данных — его будут двигать без выпуска.
async fn write_limit(tx: &deadpool_postgres::Transaction<'_>, field: &str) -> Option<i32> {
    tx.query_opt("select max_len from write_limits where field = $1", &[&field])
        .await
        .ok()
        .flatten()
        .map(|r| r.get(0))
}

/// Отказ по длине — с числами и с тем, что делать дальше.
///
/// Без «что делать» предел просто мешает: человек видит отказ и всё равно
/// вынужден угадывать. Оба выхода настоящие — вложения у тикета есть, а
/// разбиение на атомарные тикеты и есть цель предела.
fn too_long(field: &str, got: usize, max: i32) -> String {
    let what = match field {
        "title" => "the title",
        _ => "the body",
    };
    if field == "title" {
        return format!(
            "{what} is over the limit: {got} characters against {max}. A title is one line about WHAT to do; the detail belongs in the body."
        );
    }
    // Про вложения здесь НЕ говорим: ручки на сервисе есть, но ни один клиент
    // их не отдаёт — ни CLI, ни MCP. Совет «унесите во вложение» отправлял
    // человека делать невозможное, да ещё и ссылался на флаг -i из прежнего
    // JS-клиента, которого в этом CLI нет.
    format!(
        "{what} is over the limit: {got} characters against {max}. Split the work into separate \
         tickets and link them with --deps: the limit exists so a ticket can be read whole and \
         stays one unit of work."
    )
}

/// Проверяет длину значения по пределу из данных.
///
/// `previous` — длина того, что лежало в поле ДО правки. Уже записанное сверх
/// предела не становится нередактируемым: старые тикеты никто не переписывает,
/// и запретить их править значило бы заморозить их навсегда. Отказ приходит
/// только когда значение ПРЕВЫШАЕТ предел И РАСТЁТ.
async fn check_len(
    tx: &deadpool_postgres::Transaction<'_>,
    field: &str,
    value: &str,
    previous: Option<usize>,
) -> Option<String> {
    let max = write_limit(tx, field).await?;
    let got = value.chars().count();
    if got as i64 <= max as i64 {
        return None;
    }
    if previous.is_some_and(|p| got <= p) {
        return None;
    }
    Some(too_long(field, got, max))
}


// ── Векторизация: вход и книга долга ─────────────────────────────────────────

/// Предел входа embedding — 2000 Unicode scalar values.
///
/// Число совпадает с пределом записи тела, но это ДРУГАЯ ручка: запись
/// ограничена по полям (тело 2000, заголовок 256), а вход — по склейке
/// title+'\n'+body. Законный тикет даёт до 2257, поэтому вход усекается
/// отдельно. Хранимое в SQL не трогается ничем.
pub(crate) const EMBED_INPUT_MAX: usize = 2000;

/// Текст, который уйдёт в модель: заголовок ПЕРВЫМ, затем перевод строки и тело.
///
/// Порядок не косметика: при усечении выживает начало, а начало — это то, ЧТО
/// делать. Тело без заголовка опознать труднее, чем заголовок без тела.
/// Считаем в Unicode scalar values, а не в байтах: кириллица иначе усохла бы
/// вдвое против латиницы на одном и том же пределе.
pub(crate) fn embed_input(title: &str, body: &str) -> String {
    let joined = format!("{title}\n{body}");
    if joined.chars().count() <= EMBED_INPUT_MAX {
        return joined;
    }
    joined.chars().take(EMBED_INPUT_MAX).collect()
}

/// Отпечаток отправляемого входа.
///
/// Считается по ТЕКСТУ ПОСЛЕ усечения: две правки, различающиеся только за
/// пределом обрезки, дают один вход и не обязаны стоить второго обращения к
/// модели.
pub(crate) fn embed_sha(input: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(input.as_bytes());
    format!("{:x}", h.finalize())
}

/// Ставит долг индексации В ТОЙ ЖЕ транзакции, что и правка тикета.
///
/// Ничего не делает при выключенной политике: выключено — значит ноль записей,
/// ноль обращений, никаких фоновых работ.
///
/// Долг НЕ ставится, если отправляемый вход не изменился: смена приоритета,
/// исполнителя или срока не меняет title+body, а значит не стоит embedding.
/// Это и есть «неизменный отпечаток — ноль обращений к модели», и проверяется
/// оно здесь, до всякой сети.
pub(crate) async fn enqueue_upsert(
    tx: &deadpool_postgres::Transaction<'_>,
    id: &str,
) -> anyhow::Result<bool> {
    if !vector_enabled(tx).await? {
        return Ok(false);
    }
    let Some(row) = tx
        .query_opt(
            "select title, body, uuid::text from tickets where id = $1 and deleted_at is null",
            &[&id],
        )
        .await?
    else {
        return Ok(false);
    };
    let uuid: String = row.get(2);
    let sha = embed_sha(&embed_input(&row.get::<_, String>(0), &row.get::<_, String>(1)));

    // Уже в индексе с тем же отпечатком и без висящего долга — делать нечего.
    let indexed: Option<String> = tx
        .query_opt("select indexed_sha from vector_index_state where ticket_id = $1", &[&id])
        .await?
        .map(|r| r.get(0));
    let pending: bool = tx
        .query_opt("select 1 from vector_debt where ticket_id = $1", &[&id])
        .await?
        .is_some();
    if indexed.as_deref() == Some(sha.as_str()) && !pending {
        return Ok(false);
    }

    tx.execute(
        "insert into vector_debt (ticket_id, uuid, op, input_sha, attempts, last_error, queued_at)
         values ($1, $3, 'upsert', $2, 0, null, now())
         on conflict (ticket_id) do update
            set op = 'upsert', input_sha = excluded.input_sha, uuid = excluded.uuid,
                attempts = 0, last_error = null, queued_at = now()",
        &[&id, &sha, &uuid],
    )
    .await?;
    Ok(true)
}

/// Долг на удаление точки из индекса.
///
/// `done` сюда НЕ попадает: закрытый тикет остаётся живым и должен находиться.
/// Удаление — это deleted_at, и только оно.
pub(crate) async fn enqueue_delete(
    tx: &deadpool_postgres::Transaction<'_>,
    id: &str,
) -> anyhow::Result<bool> {
    if !vector_enabled(tx).await? {
        return Ok(false);
    }
    // uuid берём здесь, пока строка тикета доступна: долг обязан уметь снести
    // точку и после того, как строки не станет.
    let uuid: Option<String> = tx
        .query_opt("select uuid::text from tickets where id = $1", &[&id])
        .await?
        .map(|r| r.get(0));
    tx.execute(
        "insert into vector_debt (ticket_id, uuid, op, input_sha, attempts, last_error, queued_at)
         values ($1, $2, 'delete', null, 0, null, now())
         on conflict (ticket_id) do update
            set op = 'delete', input_sha = null, uuid = coalesce(excluded.uuid, vector_debt.uuid),
                attempts = 0, last_error = null, queued_at = now()",
        &[&id, &uuid],
    )
    .await?;
    Ok(true)
}

/// Читает политику и НЕ глотает ошибку.
///
/// Раньше здесь стоял .ok(), и отсутствие таблицы превращалось в «выключено».
/// В Postgres ошибка запроса аборти́рует транзакцию, поэтому пропустить запись
/// это не давало — но давало хуже: если бинарь выкатили раньше, чем миграции
/// прошли по схеме, КАЖДЫЙ create, patch и remove этого воркспейса отвечал 500
/// «внутренняя ошибка» БЕЗ единой строки в журнале о причине. Мой лог про долг
/// не срабатывал (ошибки-то не было, было false), а падал уже commit с
/// абстрактным «transaction is aborted».
///
/// Теперь причина доходит: в журнале будет «relation vector_policy does not
/// exist». Исход тот же 500, но искать его не придётся вслепую. Нашёл
/// glm-ntk-reviewer.
pub(crate) async fn vector_enabled(
    tx: &deadpool_postgres::Transaction<'_>,
) -> anyhow::Result<bool> {
    Ok(tx
        .query_opt("select enabled from vector_policy where only_row", &[])
        .await?
        .map(|r| r.get(0))
        .unwrap_or(false))
}

/// Whether creating should refuse on a likely duplicate, and how close counts.
///
/// Separate from `enabled` on purpose: indexing a workspace and refusing its
/// creates are different decisions with different blast radii. The threshold
/// lives here rather than in the binary because it is a property of the corpus,
/// and moving it must not need a release.
pub(crate) async fn vector_stop_policy(
    tx: &deadpool_postgres::Transaction<'_>,
) -> anyhow::Result<(bool, f64)> {
    Ok(tx
        .query_opt("select stop_on_similar, min_score from vector_policy where only_row", &[])
        .await?
        .map(|r| (r.get(0), r.get(1)))
        .unwrap_or((false, crate::vector::NEAR_DUPLICATE)))
}

#[cfg(test)]
mod embed_input_tests {
    use super::{embed_input, embed_sha, EMBED_INPUT_MAX};

    // Заголовок идёт ПЕРВЫМ и переживает усечение: при обрезке выживает начало,
    // а начало — это то, ЧТО делать.
    #[test]
    fn the_title_comes_first_and_survives_clipping() {
        let body = "тело ".repeat(5000);
        let got = embed_input("Починить выгрузку", &body);
        assert!(got.starts_with("Починить выгрузку\n"), "заголовок не первым: {}", &got[..40]);
        assert_eq!(got.chars().count(), EMBED_INPUT_MAX);
    }

    // Считаем в Unicode scalar values, а не в байтах: иначе кириллица усохла бы
    // вдвое против латиницы на одном и том же пределе.
    #[test]
    fn the_limit_counts_characters_not_bytes() {
        let cyrillic = "я".repeat(EMBED_INPUT_MAX * 2);
        let got = embed_input("", &cyrillic);
        assert_eq!(got.chars().count(), EMBED_INPUT_MAX);
        assert!(got.len() > EMBED_INPUT_MAX, "в байтах должно быть больше");
    }

    // Граница 2000/2001: ровно на пределе не режем.
    #[test]
    fn exactly_at_the_limit_is_kept_whole() {
        let body = "x".repeat(EMBED_INPUT_MAX - 2); // + заголовок "t" + '\n'
        let got = embed_input("t", &body);
        assert_eq!(got.chars().count(), EMBED_INPUT_MAX);
        assert!(got.ends_with('x'));
    }

    // Отпечаток считается ПОСЛЕ усечения: две правки, различающиеся только за
    // пределом обрезки, дают один вход и не обязаны стоить второго embedding.
    #[test]
    fn changes_beyond_the_clip_do_not_change_the_sha() {
        let a = embed_input("t", &"x".repeat(9000));
        let b = embed_input("t", &format!("{}РАЗНОЕ", "x".repeat(9000)));
        assert_eq!(embed_sha(&a), embed_sha(&b));
    }

    // А изменение внутри предела — меняет.
    #[test]
    fn a_change_inside_the_clip_changes_the_sha() {
        assert_ne!(embed_sha(&embed_input("t", "один")), embed_sha(&embed_input("t", "два")));
    }
}
