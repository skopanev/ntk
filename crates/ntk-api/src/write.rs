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
        return Err(oops(StatusCode::UNAUTHORIZED, "нужен заголовок Authorization: Bearer"));
    };
    let client = app
        .pool
        .get()
        .await
        .map_err(|_| oops(StatusCode::SERVICE_UNAVAILABLE, "база недоступна"))?;
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
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
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
    };

    match claim::next(&tx, &prefer, &pick).await {
        Ok(Some(c)) => {
            if tx.commit().await.is_err() {
                return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
            }
            // Захват больше не пишется в assignee — там владелец тикета, а не
            // тот, кто взял его в работу. Лог остаётся единственным следом
            // того, КТО взял, и потому обязателен.
            tracing::info!(actor = %actor.user_id, ticket = %c.id, workspace = %ws, "тикет взят");
            Json(json!({"id": c.id, "title": c.title, "status": c.status})).into_response()
        }
        Ok(None) => (StatusCode::NO_CONTENT, ()).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "next не прошёл");
            oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка")
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
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
    };
    match claim::start(&tx, &id).await {
        Ok(claim::StartOutcome::Taken(c)) => {
            if tx.commit().await.is_err() {
                return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
            }
            tracing::info!(actor = %actor.user_id, ticket = %c.id, workspace = %ws, "тикет взят");
            Json(json!({"id": c.id, "title": c.title, "status": c.status})).into_response()
        }
        Ok(claim::StartOutcome::AlreadyTaken { status, agent }) => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "тикет уже взят",
                "status": status,
                "agent": agent
            })),
        )
            .into_response(),
        Ok(claim::StartOutcome::NoSuchTicket) => oops(StatusCode::NOT_FOUND, "такого тикета нет"),
        Ok(claim::StartOutcome::Blocked { deps }) => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "тикет заблокирован незакрытыми зависимостями",
                "blocked_by": deps.iter()
                    .map(|(id, st)| json!({"id": id, "status": st}))
                    .collect::<Vec<_>>()
            })),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "start не прошёл");
            oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка")
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
            "body и body_append вместе не принимаются: либо заменить тело, либо дописать",
        );
    }
    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
    };

    let Ok(Some(row)) = tx.query_opt("select status from tickets where id = $1 and deleted_at is null", &[&id]).await else {
        return oops(StatusCode::NOT_FOUND, "такого тикета нет");
    };
    let current: String = row.get(0);

    if !p.force {
        match claim::requires_force(&tx, &current).await {
            Ok(true) => {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "error": format!("тикет в статусе {current} уже подобран — нужен force"),
                        "status": current
                    })),
                )
                    .into_response()
            }
            Ok(false) => {}
            Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
        }
    }

    if let Some(edits) = &p.tag_edits {
        for e in edits {
            let (sign, name) = e.split_at(1);
            let name = name.trim();
            if name.is_empty() || !matches!(sign, "+" | "-") {
                return oops(StatusCode::BAD_REQUEST, "каждый тег должен начинаться с + или -");
            }
            let sql = if sign == "+" {
                "update tickets set tags = (select array_agg(distinct t) from unnest(tags || $2::text) t) where id = $1"
            } else {
                "update tickets set tags = array_remove(tags, $2) where id = $1"
            };
            if tx.execute(sql, &[&id, &name]).await.is_err() {
                return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
            }
        }
    }

    if let Some(set) = &p.dep_set {
        if tx.execute("delete from deps where ticket_id = $1", &[&id]).await.is_err() {
            return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
        }
        for target in set {
            let target = target.trim();
            if target.is_empty() { continue; }
            if target.eq_ignore_ascii_case(&id) {
                return oops(StatusCode::BAD_REQUEST, "тикет не может ждать сам себя");
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
                        Json(json!({"error": format!("нет такого тикета: {target}")})),
                    )
                        .into_response()
                }
                Ok(_) => {}
                Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
            }
        }
    }

    if let Some(edits) = &p.dep_edits {
        for e in edits {
            if e.len() < 2 {
                return oops(StatusCode::BAD_REQUEST, "каждая зависимость должна начинаться с + или -");
            }
            let (sign, target) = e.split_at(1);
            let target = target.trim();
            if target.is_empty() || !matches!(sign, "+" | "-") {
                return oops(StatusCode::BAD_REQUEST, "каждая зависимость должна начинаться с + или -");
            }
            if sign == "+" {
                if target.eq_ignore_ascii_case(&id) {
                    return oops(StatusCode::BAD_REQUEST, "тикет не может ждать сам себя");
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
                                Json(json!({"error": format!("нет такого тикета: {target}")})),
                            )
                                .into_response();
                        }
                    }
                    Ok(_) => {}
                    Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
                }
            } else if tx
                .execute(
                    "delete from deps where ticket_id = $1 and lower(depends_on) = lower($2)",
                    &[&id, &target],
                )
                .await
                .is_err()
            {
                return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
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
                Json(json!({"error": "у тикета есть модуль: при смене проекта назовите модуль нового проекта или снимите его пустой строкой"})),
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
                Json(json!({"error": format!("модуль {m} в архиве — переназначить на него нельзя")})),
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
            return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
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
    match r {
        Ok(_) if tx.commit().await.is_ok() => {
            tracing::info!(actor = %actor.user_id, ticket = %id, workspace = %ws, forced = p.force, "тикет изменён");
            Json(json!({"id": id, "updated": true})).into_response()
        }
        _ => oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
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
    #[serde(default)]
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
}

/// Заведение тикета.
///
/// Занятый идентификатор — это 409, а не молчаливая перезапись: `on conflict
/// do nothing` вернёт ноль строк, и клиент сгенерирует другой хвост и
/// повторит. Проверка коллизий чтением всей базы уходит вместе с Notion — она
/// стоила 26 запросов и всё равно имела окно гонки между чтением и записью.
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
        return oops(StatusCode::BAD_REQUEST, "у тикета должен быть заголовок");
    }

    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
    };

    let status = p.status.clone().unwrap_or_else(|| "open".into());
    if let Some(m) = p.module.as_deref() {
        if module_is_archived(&tx, p.project.as_deref(), m).await {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({"error": format!("модуль {m} в архиве — выбрать его для новой работы нельзя")})),
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
            return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
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
                Json(json!({"error": "идентификатор занят", "id": id})),
            )
                .into_response()
        }
        Ok(_) => {}
        Err(e) => {
            // Ссылка на несуществующий статус, приоритет или исполнителя — это
            // ошибка вызывающего, а не сбой: сказать, что именно не так,
            // дешевле, чем «внутренняя ошибка» и чтение логов.
            let msg = e.to_string();
            let code = if msg.contains("foreign key") || msg.contains("check constraint") {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            tracing::error!(error = %e, "не удалось создать тикет");
            return oops(code, "не удалось создать тикет: проверьте статус, приоритет, исполнителя, модуль и формат id");
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
            Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
        }
    }
    if !missing.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": format!("нет таких тикетов: {}", missing.join(", "))})),
        )
            .into_response();
    }

    if tx.commit().await.is_err() {
        return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
    }
    tracing::info!(actor = %actor.user_id, ticket = %id, workspace = %ws, "тикет создан");
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
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
    };

    let Ok(Some(root)) = tx
        .query_opt("select id, title, status from tickets where lower(id) = lower($1) and deleted_at is null", &[&id])
        .await
    else {
        return oops(StatusCode::NOT_FOUND, "такого тикета нет");
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
                return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
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
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
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
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
    };

    let affected = tx
        .execute(
            "update tickets set deleted_at = now() where lower(id) = lower($1) and deleted_at is null",
            &[&id],
        )
        .await;
    match affected {
        Ok(0) => oops(StatusCode::NOT_FOUND, "такого тикета нет или он уже удалён"),
        Ok(_) if tx.commit().await.is_ok() => {
            tracing::info!(actor = %actor.user_id, ticket = %id, workspace = %ws, "тикет удалён");
            Json(json!({"id": id, "deleted": true, "still_waiting_on_it": waiting})).into_response()
        }
        _ => oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
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
    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
    };

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

    let people = tx
        .query("select u.id, u.kind from core.users u
                  join core.user_workspaces uw on uw.user_id = u.id
                 where uw.workspace = $1 and u.active order by u.id", &[&ws])
        .await
        .map(|rows| {
            rows.iter()
                .map(|r| json!({"id": r.get::<_, String>(0), "kind": r.get::<_, String>(1)}))
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
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
    };

    if tx
        .query_opt("select 1 from projects where id = $1", &[&project])
        .await
        .ok()
        .flatten()
        .is_none()
    {
        return oops(StatusCode::NOT_FOUND, "такого проекта нет");
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
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
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
            Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
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
                Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
            }
        } else {
            if tx.execute("delete from modules where project_id = $1 and name = $2", &[&project, m]).await.is_err() {
                return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
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
            Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
        }
    }

    for m in &added {
        if tx
            .execute("insert into modules (project_id, name) values ($1,$2) on conflict do nothing", &[&project, m])
            .await
            .is_err()
        {
            return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
        }
    }

    if tx.commit().await.is_err() {
        return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
    }
    tracing::info!(actor = %actor.user_id, project = %project, workspace = %ws,
                   added = added.len(), archived = archived.len(), deleted = deleted.len(),
                   restored = restored.len(), "реестр модулей заменён");
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
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
    };

    if tx
        .query_opt("select 1 from projects where id = $1", &[&project])
        .await
        .ok()
        .flatten()
        .is_none()
    {
        return oops(StatusCode::NOT_FOUND, "такого проекта нет");
    }

    if p.archived {
        let left: i64 = match tx
            .query_one("select count(*) from tickets where project_id = $1", &[&project])
            .await
        {
            Ok(r) => r.get(0),
            Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
        };
        if left > 0 {
            return (
                StatusCode::CONFLICT,
                Json(json!({
                    "error": format!(
                        "в проекте {project} ещё {left} тикетов: перенесите их, иначе работа станет невидимой, оставшись живой"
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
        return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
    }
    if tx.commit().await.is_err() {
        return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
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
        return oops(StatusCode::BAD_REQUEST, "назовите целевой проект");
    }
    if to == from {
        return oops(StatusCode::BAD_REQUEST, "перенос в тот же проект ничего не значит");
    }
    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
    };

    if tx
        .query_opt("select 1 from projects where id = $1", &[&from])
        .await
        .ok()
        .flatten()
        .is_none()
    {
        return oops(StatusCode::NOT_FOUND, "такого проекта нет");
    }
    if tx
        .execute(
            "insert into projects (id, name) values ($1, $1) on conflict (id) do nothing",
            &[&to],
        )
        .await
        .is_err()
    {
        return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
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
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
    };
    let target: Vec<String> = match tx
        .query("select name from modules where project_id = $1", &[&to])
        .await
    {
        Ok(rows) => rows.iter().map(|r| r.get::<_, String>(0)).collect(),
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
    };
    let missing = modules_missing_in_target(&source, &target);
    if !missing.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!(
                    "в проекте {to} не заведены модули, на которые ссылаются переносимые тикеты: {}",
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
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
    };
    if tx.commit().await.is_err() {
        return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
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
        return oops(StatusCode::BAD_REQUEST, "назовите хотя бы один модуль");
    }

    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
    };
    if tx
        .query_opt("select 1 from projects where id = $1", &[&project])
        .await
        .ok()
        .flatten()
        .is_none()
    {
        return oops(StatusCode::NOT_FOUND, "такого проекта нет");
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
            Err(_) => return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
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
                    return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
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
                    return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
                }
                added.push(m.clone());
            }
        }
    }
    if tx.commit().await.is_err() {
        return oops(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
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
