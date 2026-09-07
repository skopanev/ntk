//! Обход тикетов по одному: просмотр, а не работа.
//!
//! `next` берёт тикет В РАБОТУ — меняет статус и ставит исполнителя. Для
//! проверки это негодно: пройти так тридцать тикетов значит перевести их все на
//! себя в in_progress, то есть испортить очередь, а не проверить её. Здесь
//! ничего не меняется, кроме отметки «показано».
//!
//! Список не снимается заранее: каждый шаг спрашивает базу заново и берёт
//! первый непоказанный. Со снимком набора первая же правка сдвигает остальных,
//! и шаг перескакивает через соседа — молча, что и есть худший способ потерять
//! тикет при проверке.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;

use crate::{auth, db, filter, App};

#[derive(Deserialize)]
pub struct WalkQuery {
    workspace: Option<String>,
    /// Идентификатор сеанса. Придумывает КЛИЕНТ.
    ///
    /// Именно поэтому два агента под одним ключом не мешают друг другу: у
    /// каждого свой сеанс. Ключ на человека означал бы общую память обхода и
    /// один и тот же тикет обоим.
    walk_id: String,
    /// Забыть показанное и пойти сначала.
    #[serde(default)]
    reset: bool,

    // Отборы перечислены поштучно, а не вложены через `#[serde(flatten)]`.
    //
    // С flatten разбор строки запроса ломается молча: под ним всё поле
    // становится строкой, и `strict=true` перестаёт быть логическим значением —
    // «invalid type: string "true", expected a boolean». Ошибка вылезает только
    // на живом запросе, компилятор её не видит.
    status: Option<String>,
    tag: Option<String>,
    #[serde(default)]
    strict: bool,
    title: Option<String>,
    assignee: Option<String>,
    project: Option<String>,
    module: Option<String>,
    #[serde(default)]
    all: bool,
}

impl WalkQuery {
    fn filters(&self) -> filter::Filters {
        filter::Filters {
            module: self.module.clone(),
            status: self.status.clone(),
            tag: self.tag.clone(),
            strict: self.strict,
            title: self.title.clone(),
            assignee: self.assignee.clone(),
            project: self.project.clone(),
            all: self.all,
        }
    }
}

fn err(c: StatusCode, m: &str) -> Response {
    (c, Json(json!({ "error": m }))).into_response()
}

pub async fn step(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
    Query(q): Query<WalkQuery>,
) -> Response {
    let Some(key) = auth::bearer(&headers) else {
        return err(StatusCode::UNAUTHORIZED, "нужен заголовок Authorization: Bearer");
    };
    let Ok(mut client) = app.pool.get().await else {
        return err(StatusCode::SERVICE_UNAVAILABLE, "база недоступна");
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

    let f = q.filters();
    let what = f.describe();
    let user = actor.user_id.clone();

    // Показанное живёт в core, а тикеты — в схеме воркспейса. Читаем сначала,
    // пока роль ещё наша: под ролью воркспейса в core уже не заглянуть.
    if q.reset {
        // Владелец в условии: чужой сеанс не сбрасывается даже по угаданному
        // идентификатору.
        if let Err(e) = client
            .execute(
                "delete from core.walk_state where walk_id=$1 and user_id=$2",
                &[&q.walk_id, &user],
            )
            .await
        {
            tracing::error!(error = %e, "сброс обхода не прошёл");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
        }
    }
    // Брошенные обходы убираются здесь же, а не отдельным заданием по
    // расписанию: задание — ещё одна вещь, которая однажды молча перестанет
    // запускаться, и мы узнаем об этом по распухшей таблице. Неделя выбрана
    // потому, что через неделю «я это уже смотрел» всё равно неправда:
    // статусы разъехались, половина тикетов закрыта. Удаление идёт по индексу
    // на updated_at и стоит доли миллисекунды.
    let _ = client
        .execute(
            "delete from core.walk_state where updated_at < now() - interval '7 days'",
            &[],
        )
        .await;

    // Заводим сеанс, если его нет. Существующий чужой не перехватывается:
    // владелец сверяется ниже, и несовпадение — отказ, а не тихая подмена.
    if let Err(e) = client
        .execute(
            "insert into core.walk_state (walk_id, user_id, workspace, what)
             values ($1,$2,$3,$4) on conflict (walk_id) do nothing",
            &[&q.walk_id, &user, &ws, &what],
        )
        .await
    {
        tracing::error!(error = %e, "сеанс обхода не завёлся");
        return err(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
    }
    let seen: Vec<String> = match client
        .query_opt(
            "select seen from core.walk_state where walk_id=$1 and user_id=$2",
            &[&q.walk_id, &user],
        )
        .await
    {
        Ok(Some(r)) => r.get(0),
        Ok(None) => return err(StatusCode::FORBIDDEN, "этот обход принадлежит другому"),
        Err(e) => {
            tracing::error!(error = %e, "чтение обхода не прошло");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
        }
    };

    let bound = f.bind(&actor.user_id);
    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, workspace = %ws, "не удалось войти в воркспейс");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
        }
    };

    let mut count_sql = String::from("select count(*) from tickets where deleted_at is null");
    let mut count_args: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();
    filter::apply(&mut count_sql, &mut count_args, &bound);
    let total: i64 = match tx.query_one(&count_sql, &count_args).await {
        Ok(r) => r.get(0),
        Err(e) => {
            tracing::error!(error = %e, "счёт для обхода не прошёл");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
        }
    };

    let mut sql = String::from(
        "select id, uuid::text, title, status, priority, type, assignee,
                project_id, module, tags, body,
                created_at::text, updated_at::text,
                started_at::text, closed_at::text
           from tickets where deleted_at is null",
    );
    let mut args: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();
    filter::apply(&mut sql, &mut args, &bound);
    args.push(&seen);
    // Порядок тот же, что у списка: человек проверяет в том порядке, в каком
    // видел.
    sql.push_str(&format!(
        " and id <> all(${}::text[]) order by created_at desc limit 1",
        args.len()
    ));

    let row = match tx.query_opt(&sql, &args).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "шаг обхода не прошёл");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
        }
    };
    let _ = tx.commit().await;

    let Some(r) = row else {
        return (
            StatusCode::OK,
            Json(json!({
                "done": true,
                "seen": seen.len(),
                "total": total,
                "what": what,
                "hint": "обход пройден; начать заново — reset"
            })),
        )
            .into_response();
    };

    let ticket = ntk_core::Ticket {
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
        body: r.get(10),
        due: None,
        created_at: r.get(11),
        updated_at: r.get(12),
        started_at: r.get(13),
        closed_at: r.get(14),
    };

    // Отметка ставится проверкой-и-записью: условие `not (… = any(seen))`
    // означает, что запись пройдёт только у того, кто пришёл первым.
    //
    // Это страховка на случай, когда одним идентификатором сеанса идут двое.
    // Само по себе такое не случается — идентификатор придумывает клиент, — но
    // если случится, проигравший получит ОТКАЗ, а не тот же тикет молча.
    let marked = client
        .execute(
            "update core.walk_state
                set seen = seen || $3::text, updated_at = now()
              where walk_id = $1 and user_id = $2 and not ($3 = any(seen))",
            &[&q.walk_id, &user, &ticket.id],
        )
        .await;
    match marked {
        Ok(0) => {
            return (
                StatusCode::CONFLICT,
                Json(json!({"error": "этот тикет только что взял другой обход с тем же walk_id — повторите шаг"})),
            )
                .into_response()
        }
        Ok(_) => {}
        Err(e) => {
            tracing::error!(error = %e, "отметка обхода не записалась");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "внутренняя ошибка");
        }
    }

    (
        StatusCode::OK,
        Json(json!({
            "ticket": ticket,
            "at": seen.len() + 1,
            "total": total,
            "what": what
        })),
    )
        .into_response()
}
