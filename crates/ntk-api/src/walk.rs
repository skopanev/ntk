//! Walking tickets one at a time: reviewing, not working.
//!
//! `next` takes a ticket INTO WORK — it changes the status and sets the
//! assignee. That is useless for review: walking thirty tickets that way would
//! move all thirty onto yourself in in_progress, wrecking the queue rather than
//! checking it. Nothing changes here except the "shown" mark.
//!
//! The list is not snapshotted up front: every step asks the database again and
//! takes the first unshown one. With a snapshot, the first edit shifts the rest
//! and a step skips over a neighbour — silently, which is the worst way to lose
//! a ticket during review.

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

/// Неизвестный параметр — отказ, по той же причине, что и у списка.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WalkQuery {
    workspace: Option<String>,
    /// The session identifier. Invented by the CLIENT.
    ///
    /// That is exactly why two agents under one key do not disturb each other:
    /// each has its own session. Keying on the person would mean a shared walk
    /// memory and the same ticket handed to both.
    walk_id: String,
    /// Forget what was shown and start over.
    #[serde(default)]
    reset: bool,

    // The filters are listed one by one rather than nested via
    // `#[serde(flatten)]`.
    //
    // With flatten, query-string parsing breaks silently: under it every field
    // becomes a string and `strict=true` stops being a boolean — "invalid type:
    // string \"true\", expected a boolean". The error only shows up on a live
    // request; the compiler does not see it.
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
    /// Отбор по датам: `created_at:gte:2026-09-01,created_at:lte:2026-09-12`.
    date: Option<String>,
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
            date: self.date.clone(),
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
        return err(StatusCode::UNAUTHORIZED, "an Authorization: Bearer header is required");
    };
    let Ok(mut client) = app.pool.get().await else {
        return err(StatusCode::SERVICE_UNAVAILABLE, "the database is unavailable");
    };
    let actor = match auth::resolve(&client, key).await {
        Ok(Some(a)) => a,
        Ok(None) => return err(StatusCode::UNAUTHORIZED, "unknown or revoked key"),
        Err(e) => {
            tracing::error!(error = %e, "не удалось разрешить ключ");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
        }
    };
    let Some(ws) = q.workspace.clone() else {
        return err(StatusCode::BAD_REQUEST, "name a workspace — there is no default");
    };
    if !actor.may_enter(&ws) {
        return err(StatusCode::FORBIDDEN, "this key gives no access to that workspace");
    }

    let f = q.filters();
    let what = f.describe();
    let user = actor.user_id.clone();

    // What has been shown lives in core, while the tickets live in the
    // workspace schema. We read it first, while the role is still ours: under
    // the workspace role there is no looking into core any more.
    if q.reset {
        // The owner is in the condition: somebody else's session is not reset
        // even with a guessed identifier.
        if let Err(e) = client
            .execute(
                "delete from core.walk_state where walk_id=$1 and user_id=$2",
                &[&q.walk_id, &user],
            )
            .await
        {
            tracing::error!(error = %e, "сброс обхода не прошёл");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
        }
    }
    // Abandoned walks are cleared right here rather than by a scheduled job: a
    // job is one more thing that will one day quietly stop running, and we
    // would learn about it from a bloated table. A week was chosen because
    // after a week "I already looked at this" is untrue anyway: the statuses
    // have moved on and half the tickets are closed. The delete goes through
    // the index on updated_at and costs a fraction of a millisecond.
    let _ = client
        .execute(
            "delete from core.walk_state where updated_at < now() - interval '7 days'",
            &[],
        )
        .await;

    // Create the session if it does not exist. An existing one belonging to
    // somebody else is not hijacked: the owner is checked below, and a mismatch
    // is a refusal rather than a quiet substitution.
    if let Err(e) = client
        .execute(
            "insert into core.walk_state (walk_id, user_id, workspace, what)
             values ($1,$2,$3,$4) on conflict (walk_id) do nothing",
            &[&q.walk_id, &user, &ws, &what],
        )
        .await
    {
        tracing::error!(error = %e, "сеанс обхода не завёлся");
        return err(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
    }
    let seen: Vec<String> = match client
        .query_opt(
            "select seen from core.walk_state where walk_id=$1 and user_id=$2",
            &[&q.walk_id, &user],
        )
        .await
    {
        Ok(Some(r)) => r.get(0),
        Ok(None) => return err(StatusCode::FORBIDDEN, "this walk belongs to somebody else"),
        Err(e) => {
            tracing::error!(error = %e, "чтение обхода не прошло");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
        }
    };

    let bound = match f.bind(&actor.user_id) {
        Ok(b) => b,
        Err(e) => return err(StatusCode::BAD_REQUEST, &e),
    };
    let tx = match db::begin(&mut client, &ws).await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!(error = %e, workspace = %ws, "не удалось войти в воркспейс");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
        }
    };

    let mut count_sql = String::from("select count(*) from tickets where deleted_at is null");
    let mut count_args: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();
    filter::apply(&mut count_sql, &mut count_args, &bound);
    let total: i64 = match tx.query_one(&count_sql, &count_args).await {
        Ok(r) => r.get(0),
        Err(e) => {
            tracing::error!(error = %e, "счёт для обхода не прошёл");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
        }
    };

    let mut sql = String::from(
        "select id, uuid::text, title, status, priority, type, assignee,
                project_id, module, tags, body,
                created_at::text, updated_at::text,
                started_at::text, closed_at::text, current_status_at::text
           from tickets where deleted_at is null",
    );
    let mut args: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = Vec::new();
    filter::apply(&mut sql, &mut args, &bound);
    args.push(&seen);
    // The same order as the list: a person reviews in the order they saw.
    sql.push_str(&format!(
        " and id <> all(${}::text[]) order by created_at desc limit 1",
        args.len()
    ));

    let row = match tx.query_opt(&sql, &args).await {
        Ok(r) => r,
        Err(e) => {
            tracing::error!(error = %e, "шаг обхода не прошёл");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
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
                "hint": "the walk is finished; reset to start over"
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
        current_status_at: r.get(15),
    };

    // The mark is set by check-and-write: the `not (… = any(seen))` condition
    // means the write only succeeds for whoever got there first.
    //
    // This is insurance for the case where two walkers share one session
    // identifier. It does not happen on its own — the client invents the
    // identifier — but if it does, the loser gets a REFUSAL rather than the
    // same ticket in silence.
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
                Json(json!({"error": "another walk with the same walk_id just took this ticket — repeat the step"})),
            )
                .into_response()
        }
        Ok(_) => {}
        Err(e) => {
            tracing::error!(error = %e, "отметка обхода не записалась");
            return err(StatusCode::INTERNAL_SERVER_ERROR, "internal error");
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
