//! ntk-api — единственное, что смотрит наружу.
//!
//! Postgres слушает только localhost; клиенты предъявляют ключ воркспейса, а
//! не креды базы. Отсюда же следует, что пулер не нужен: к базе подключается
//! один этот сервис со своим пулом, сколько бы агентов ни ходило по HTTP.

mod auth;
mod config;
mod db;
mod claim;
mod device;
mod enroll;
mod oauth;
mod attach;
mod filter;
mod mcp_http;
mod mcp_oauth;
mod release;
mod spaces;
mod walk;
mod write;

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    routing::{patch, post},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

pub struct App {
    pub pool: deadpool_postgres::Pool,
    pub cfg: config::Config,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Конфиг проверяется здесь и падает здесь: неверный домен обязан ронять
    // старт, а не отсекать людей молча в рантайме.
    let cfg = config::Config::from_env()?;
    let port = cfg.port;
    let pool = db::pool(&cfg.database_url)?;

    // Соединение проверяем до того, как объявим себя живыми.
    let probe = pool.get().await?;
    let who: String = probe.query_one("select current_user", &[]).await?.get(0);
    drop(probe);
    tracing::info!(
        user = %who,
        domains = ?cfg.google_hd,
        redirect_uri = %cfg.redirect_uri(),
        "подключение к базе установлено"
    );

    let app = Arc::new(App { pool, cfg });
    let router = Router::new()
        .route("/health", get(health))
        // Без ключа намеренно: клиент, который ещё не вошёл, тоже должен
        // уметь обновиться, а номер версии секретом не является.
        .route("/v1/version", get(release::current))
        .route("/v1/download", get(release::download))
        .route("/v1/me", get(whoami))
        .route("/v1/tickets", get(tickets).post(write::create))
        .route("/v1/tickets/{id}", get(ticket_one).delete(write::remove))
        .route("/v1/tickets/{id}/deps", get(write::deps))
        .route("/v1/meta", get(write::meta))
        .route("/v1/projects/{id}/modules", axum::routing::put(write::set_modules))
        // Жизненный цикл проекта: убрать опустевшее имя из выбора и
        // перенести тикеты целиком. Переименования нет намеренно —
        // id проекта сидит префиксом в первичных ключах тикетов.
        .route("/v1/projects/{id}", patch(write::patch_project))
        .route("/v1/projects/{id}/move", post(write::move_project))
        // Вход без передачи ключа из рук в руки: устройство берёт код,
        // человек проходит Google, устройство забирает ключ.
        // Захват тикета — то, ради чего переезжали: в Notion между чтением
        // статуса и записью было окно, и два агента брали один тикет.
        .route("/v1/tickets/next", post(write::next))
        // Обход для проверки: показывает следующий непросмотренный и НИЧЕГО не
        // меняет в тикете, в отличие от next.
        .route("/v1/tickets/walk", post(walk::step))
        .route("/v1/tickets/{id}/start", post(write::start))
        .route("/v1/tickets/{id}", patch(write::patch))
        // Вложения: байты идут между клиентом и Spaces напрямую, мимо нас.
        .route("/v1/tickets/{id}/attachments", get(attach::list).post(attach::begin))
        .route("/v1/tickets/{id}/attachments/commit", post(attach::commit))
        .route("/device/start", post(enroll::start))
        .route("/device/poll", post(enroll::poll))
        .route("/link", get(enroll::link))
        .route("/auth/google/callback", get(enroll::callback))
        // Удалённый MCP: Claude Desktop подключается по адресу, без бинаря на
        // машине человека. Метаданные отдаются без ключа намеренно — клиент
        // читает их ДО того, как ему есть что предъявить.
        .route("/.well-known/oauth-protected-resource", get(mcp_oauth::protected_resource))
        .route("/.well-known/oauth-protected-resource/mcp-claude", get(mcp_oauth::protected_resource))
        .route("/.well-known/oauth-authorization-server", get(mcp_oauth::authorization_server))
        .route("/oauth/register", post(mcp_oauth::register))
        .route("/oauth/authorize", get(mcp_oauth::authorize))
        .route("/oauth/google/callback", get(mcp_oauth::callback))
        .route("/oauth/token", post(mcp_oauth::token))
        .route(mcp_oauth::MCP_PATH, post(mcp_http::endpoint).get(mcp_http::endpoint_get))
        .with_state(app);

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    tracing::info!(%port, "слушаю на localhost — наружу смотрит только Caddy");
    axum::serve(listener, router).await?;
    Ok(())
}

/// Здоровье проверяется запросом к базе, а не фактом, что процесс жив:
/// «отвечает» и «работает» — разные утверждения.
async fn health(State(app): State<Arc<App>>) -> Response {
    match app.pool.get().await {
        Ok(c) => match c.query_one("select 1", &[]).await {
            Ok(_) => (StatusCode::OK, Json(json!({"status": "ok"}))).into_response(),
            Err(e) => unhealthy(&e.to_string()),
        },
        Err(e) => unhealthy(&e.to_string()),
    }
}

fn unhealthy(reason: &str) -> Response {
    tracing::error!(%reason, "база недоступна");
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"status": "degraded", "reason": "database"})),
    )
        .into_response()
}

/// Кто я и куда мне можно.
///
/// Без этого человек, вошедший через браузер, не знает, что подставлять в -W:
/// список воркспейсов лежит на сервере и наружу не отдавался. Первый же вопрос
/// нового пользователя — «а какие есть?».
pub(crate) async fn whoami(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    let Some(key) = auth::bearer(&headers) else {
        return err(StatusCode::UNAUTHORIZED, "нужен заголовок Authorization: Bearer");
    };
    let Ok(client) = app.pool.get().await else {
        return unhealthy("база недоступна");
    };
    match auth::resolve(&client, key).await {
        Ok(Some(a)) => Json(json!({"user_id": a.user_id, "workspaces": a.workspaces})).into_response(),
        Ok(None) => err(StatusCode::UNAUTHORIZED, "ключ неизвестен или отозван"),
        Err(_) => err(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
    }
}

#[derive(Deserialize)]
struct TicketsQuery {
    workspace: Option<String>,
    status: Option<String>,
    /// По умолчанию показываем только СВОИ тикеты: очередь большая, а вопрос
    /// «что на мне» задают чаще, чем «что вообще есть». `all=true` снимает.
    #[serde(default)]
    all: bool,
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
    /// Отбор по тегам через запятую. Все перечисленные должны быть на тикете:
    /// «и», а не «или».
    tag: Option<String>,
    /// Тег должен совпасть ЦЕЛИКОМ, а не войти частью.
    ///
    /// По умолчанию ищем вхождение: теги живут семействами — `infra` и
    /// `initiative:infra` про одно и то же, и человек, спрашивающий про
    /// infra, хочет оба. Точное совпадение остаётся под флагом, потому что
    /// иногда нужен именно один тег из семейства.
    ///
    /// Цена вхождения — скан вместо GIN-индекса: `@>` по массиву индекс берёт,
    /// а поиск подстроки внутри элементов — нет. На 2–5 тысячах тикетов это
    /// единицы миллисекунд; станет дорого — поставлю триграммный индекс, но по
    /// замеру, а не заранее.
    #[serde(default)]
    strict: bool,
    /// Отбор по исполнителю. Не путать с `all`: `all=false` означает «мои»,
    /// а этот параметр — «чьи именно», и нужен, чтобы смотреть чужую очередь.
    assignee: Option<String>,
    /// Отбор по проекту.
    project: Option<String>,
    /// Отбор по модулю.
    module: Option<String>,
    /// Отбор по заголовку: вхождение подстроки, регистр не важен.
    ///
    /// Индекса под это нет — нужен триграммный, а он стоит места и записи.
    /// Ставить его вслепую не стал: 2–5 тысяч заголовков на воркспейс сканируются
    /// за единицы миллисекунд, а решать надо по замеру, а не по опасению.
    title: Option<String>,
    /// Вернуть ТОЛЬКО число подходящих, без самих тикетов.
    ///
    /// Без этого сосчитать больше страницы было нечем: приходилось листать
    /// `ntk ls` пачками по 500 и складывать. Потолок в 500 — защита от выдачи
    /// мегабайтов, а не ответ на вопрос «сколько всего».
    #[serde(default)]
    count: bool,
}

fn default_limit() -> i64 {
    50
}

/// Список тикетов. Пагинация обязательна и по умолчанию узкая: в Notion этот
/// же вызов без ограничений отдавал 1 139 642 байта, чего не выдерживает ни
/// один контекст модели.
/// Один тикет по id, с зависимостями.
///
/// Отдельная ручка, а не фильтр по списку: `show` — это единственное место,
/// где зависимости обязаны быть настоящими. В списке они не заполняются, и для
/// списка это терпимо; для одного тикета «deps пусто» означало бы «ни от чего
/// не зависит», что неправда и что уже однажды стоило нам разбирательства.
pub(crate) async fn ticket_one(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Query(q): Query<TicketsQuery>,
) -> Response {
    let Some(key) = auth::bearer(&headers) else {
        return err(StatusCode::UNAUTHORIZED, "нужен заголовок Authorization: Bearer");
    };
    let mut client = match app.pool.get().await {
        Ok(c) => c,
        Err(e) => return unhealthy(&e.to_string()),
    };
    let actor = match auth::resolve(&client, key).await {
        Ok(Some(a)) => a,
        Ok(None) => return err(StatusCode::UNAUTHORIZED, "ключ неизвестен или отозван"),
        Err(e) => {
            tracing::error!(error = %e, "не удалось разрешить ключ");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
        }
    };
    let Some(ws) = q.workspace.clone() else {
        return err(StatusCode::BAD_REQUEST, "укажите workspace — значения по умолчанию нет");
    };
    if !actor.may_enter(&ws) {
        return err(StatusCode::FORBIDDEN, "ключ не даёт доступа к этому воркспейсу");
    }

    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, workspace = %ws, "не удалось войти в воркспейс");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
        }
    };

    // Регистр в идентификаторах разный — BQ_Analitycs-kvtzwecu5j соседствует с
    // proj-xxxx — поэтому сравнение регистронезависимое, под индекс lower(id).
    let row = match tx
        .query_opt(
            "select id, uuid::text, title, status, priority, type, assignee,
                    project_id, module, tags, body, due::text,
                    created_at::text, updated_at::text,
                    started_at::text, closed_at::text
               from tickets where lower(id) = lower($1) and deleted_at is null",
            &[&id],
        )
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "не удалось прочитать тикет");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
        }
    };
    let Some(r) = row else {
        return err(StatusCode::NOT_FOUND, "тикета нет в этом воркспейсе");
    };

    let ticket_id: String = r.get(0);
    let deps = match tx
        .query("select depends_on from deps where ticket_id = $1 order by depends_on", &[&ticket_id])
        .await
    {
        Ok(rows) => rows.iter().map(|d| d.get::<_, String>(0)).collect(),
        Err(e) => {
            tracing::error!(error = %e, "не удалось прочитать зависимости");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
        }
    };

    let ticket = ntk_core::Ticket {
        id: ticket_id,
        uuid: r.get(1),
        title: r.get(2),
        status: r.get(3),
        priority: r.get(4),
        kind: r.get(5),
        assignee: r.get(6),
        project: r.get(7),
        module: r.get(8),
        tags: r.get(9),
        deps,
        body: r.get(10),
        due: r.get(11),
        created_at: r.get(12),
        updated_at: r.get(13),
        started_at: r.get(14),
        closed_at: r.get(15),
    };
    (StatusCode::OK, Json(serde_json::json!({ "ticket": ticket }))).into_response()
}

pub(crate) async fn tickets(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Query(q): Query<TicketsQuery>,
) -> Response {
    let Some(key) = auth::bearer(&headers) else {
        return err(StatusCode::UNAUTHORIZED, "нужен заголовок Authorization: Bearer");
    };

    let mut client = match app.pool.get().await {
        Ok(c) => c,
        Err(e) => return unhealthy(&e.to_string()),
    };

    let actor = match auth::resolve(&client, key).await {
        Ok(Some(a)) => a,
        Ok(None) => return err(StatusCode::UNAUTHORIZED, "ключ неизвестен или отозван"),
        Err(e) => {
            tracing::error!(error = %e, "не удалось разрешить ключ");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
        }
    };

    // Воркспейс называется явно. Понятия «по умолчанию» нет намеренно: раньше
    // забытый -W молча уводил запись в чужой воркспейс.
    let Some(ws) = q.workspace.clone() else {
        return err(
            StatusCode::BAD_REQUEST,
            "укажите workspace — значения по умолчанию нет",
        );
    };
    if !actor.may_enter(&ws) {
        return err(StatusCode::FORBIDDEN, "ключ не даёт доступа к этому воркспейсу");
    }

    let limit = q.limit.clamp(1, 500);
    let offset = q.offset.max(0);
    // Статус — СПИСОК, как и теги: «покажи открытые и заблокированные» —
    // обычный вопрос, и старый инструмент на него отвечал (JS, 684b8fb).
    // Переписывание на Rust это потеряло, а `status = $1` с "open,blocked"
    // молча отдавал ноль: ответ, неотличимый от «ничего не подходит».
    let statuses: Option<Vec<String>> = q.status.as_deref().map(|t| {
        t.split(',').map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()
    });
    let statuses = statuses.filter(|v: &Vec<String>| !v.is_empty());
    let module = q.module.clone();
    // Несколько тегов — «и»: спрашивают обычно «что и то, и другое», а не
    // «хоть что-нибудь из».
    let tags: Option<Vec<String>> = q.tag.as_deref().map(|t| {
        t.split(',').map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect()
    });
    let tags = tags.filter(|v: &Vec<String>| !v.is_empty());
    let title = q.title.clone();
    // Фильтр по исполнителю ставит СЕРВЕР из ключа, а не клиент из флага:
    // иначе «мои тикеты» стали бы просьбой, которую можно не выполнять.
    // Явно названный исполнитель важнее умолчания «мои»: спросили про чужую
    // очередь — отвечаем про чужую.
    let mine: Option<String> = match (&q.assignee, q.all) {
        (Some(a), _) => Some(a.clone()),
        (None, true) => None,
        (None, false) => Some(actor.user_id.clone()),
    };
    let project = q.project.clone();

    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, workspace = %ws, "не удалось войти в воркспейс");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
        }
    };

    // Опечатка обязана отличаться от ответа.
    //
    // `-P ws-a` в воркспейсе, где проект называется иначе, отдавал ноль — ту
    // же цифру, что и честное «таких тикетов нет». Одно это ответ, другое
    // промах, и их сделали неразличимыми молча. В JS-инструменте проверка была
    // (94ff114), при переписывании на Rust потерялась вместе с запятыми.
    if let Some(names) = &statuses {
        let known: Vec<String> = match tx.query("select name from statuses", &[]).await {
            Ok(rows) => rows.iter().map(|r| r.get::<_, String>(0)).collect(),
            Err(_) => return err(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
        };
        let unknown = write::absent_from(names, &known);
        if !unknown.is_empty() {
            let mut all = known.clone();
            all.sort();
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!(
                        "неизвестный статус: {}. Есть: {}",
                        unknown.join(", "),
                        all.join(", ")
                    ),
                    "unknown_statuses": unknown,
                    "statuses": all
                })),
            )
                .into_response();
        }
    }
    if let Some(pr) = &project {
        let known: Vec<String> = match tx.query("select id from projects", &[]).await {
            Ok(rows) => rows.iter().map(|r| r.get::<_, String>(0)).collect(),
            Err(_) => return err(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка"),
        };
        let asked = vec![pr.clone()];
        if !write::absent_from(&asked, &known).is_empty() {
            // Близкие имена важнее полного списка: их обычно и имели в виду, а
            // список проектов бывает в сотню строк.
            let near = write::near_misses(pr, &known);
            let hint = if near.is_empty() {
                String::from("Список: ntk meta")
            } else {
                format!("Близкие: {}", near.join(", "))
            };
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": format!("неизвестный проект \"{pr}\". {hint}"),
                    "unknown_project": pr,
                    "near": near
                })),
            )
                .into_response();
        }
    }

    // Условия собираются, а не прячутся за «или».
    //
    // Раньше здесь было `where ($1::text is null or status = $1)` — коротко и
    // удобно, но такая конструкция ОТКЛЮЧАЕТ индексы: планировщик не может
    // доказать, что частичный индекс подойдёт, когда условие спрятано за or.
    // Замерено на 5340 тикетах: последовательный скан всей таблицы, 4179 строк
    // на каждый список. При росте это линейно дороже.
    let head = if q.count {
        "select count(*)"
    } else {
        "select id, uuid::text, title, status, priority, type, assignee,
                project_id, module, tags, body,
                created_at::text, updated_at::text,
                started_at::text, closed_at::text"
    };
    let mut sql = format!("{head} from tickets where deleted_at is null");
    let mut args: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();
    // `= any(...)` вместо цепочки `or`: одно условие, индекс по статусу
    // остаётся применимым — ровно та причина, по которой выше отказались от
    // «или» в пользу собранных условий.
    if statuses.is_some() {
        args.push(&statuses);
        sql.push_str(&format!(" and status = any(${})", args.len()));
    }
    if mine.is_some() {
        args.push(&mine);
        sql.push_str(&format!(" and assignee = ${}", args.len()));
    }
    if module.is_some() {
        args.push(&module);
        sql.push_str(&format!(" and module = ${}", args.len()));
    }
    match (&tags, q.strict) {
        (Some(_), true) => {
            args.push(&tags);
            // Массив и `@>`, а не `= any(...)`: первое берёт GIN-индекс, второе
            // уходит в скан таблицы. Та же ошибка однажды заставляла список
            // читать 4179 строк на каждый вызов.
            sql.push_str(&format!(" and tags @> ${}::text[]", args.len()));
        }
        (Some(list), false) => {
            // Каждый названный тег обязан войти хотя бы в один тег тикета:
            // «и» между названными, вхождение внутри каждого.
            for t in list {
                args.push(t);
                sql.push_str(&format!(
                    " and exists (select 1 from unnest(tags) x where x ilike '%' || ${} || '%')",
                    args.len()
                ));
            }
        }
        (None, _) => {}
    }
    if project.is_some() {
        args.push(&project);
        sql.push_str(&format!(" and project_id = ${}", args.len()));
    }
    if title.is_some() {
        args.push(&title);
        sql.push_str(&format!(" and title ilike '%' || ${} || '%'", args.len()));
    }

    // Счёт не листается: ни порядка, ни страницы ему не нужно.
    if q.count {
        let n: i64 = match tx.query_one(&sql, &args).await {
            Ok(r) => r.get(0),
            Err(e) => {
                tracing::error!(error = %e, workspace = %ws, "счёт тикетов не прошёл");
                return err(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
            }
        };
        return (StatusCode::OK, Json(serde_json::json!({ "count": n }))).into_response();
    }

    args.push(&limit);
    sql.push_str(&format!(" order by created_at desc limit ${}", args.len()));
    args.push(&offset);
    sql.push_str(&format!(" offset ${}", args.len()));

    let out = tx
        .query(&sql, &args)
        .await
        .map(|rows| {
            rows.iter()
                .map(|r| ntk_core::Ticket {
                    id: r.get(0),
                    uuid: r.get(1),
                    title: r.get(2),
                    status: r.get(3),
                    priority: r.get(4),
                    kind: r.get(5),
                    assignee: r.get(6),
                    project: r.get(7),
                    module: r.get(8),
                    tags: r.get(9),
                    deps: Vec::new(),
                    due: None,
                    body: r.get(10),
                    created_at: r.get(11),
                    updated_at: r.get(12),
                    started_at: r.get(13),
                    closed_at: r.get(14),
                })
                .collect::<Vec<_>>()
        });

    match out {
        Ok(tickets) => (StatusCode::OK, Json(json!({"tickets": tickets}))).into_response(),
        Err(e) => {
            tracing::error!(error = %e, workspace = %ws, "запрос тикетов не прошёл");
            err(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка")
        }
    }
}

fn err(code: StatusCode, message: &str) -> Response {
    (code, Json(json!({"error": message}))).into_response()
}
