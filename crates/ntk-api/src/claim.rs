//! Claiming a ticket — the thing the whole migration was for.
//!
//! In the previous system there was a window between reading the status and
//! writing it: two agents saw the same `open` and both wrote `in_progress`. It
//! offered no conditional write, and there was nothing to work around it with.
//! Here the claim is atomic, and there are two mechanisms because the jobs
//! differ.

use tokio_postgres::Transaction;

pub struct Claimed {
    pub id: String,
    pub title: String,
    pub status: String,
}

/// `next` — take any free ticket, optionally preferring tags.
///
/// IMPORTANT: claiming does NOT touch `assignee`. That field answers "who is
/// responsible for this ticket", not "who is holding it right now"; rewriting
/// it on claim would silently erase the responsible person. A claim changes
/// only the status — the move to `in_progress` IS the claim.
///
/// `FOR UPDATE SKIP LOCKED`: parallel agents work through the queue without
/// blocking each other. Whoever got the row works on it; the others see the
/// next one instead of waiting.
///
/// `prefer` sets an order of preference: `["infra","alpha"]` means "everything
/// from infra first, then alpha, then the rest". An order, not a filter —
/// otherwise an agent would idle on an empty lane instead of taking the next
/// most important thing.
///
/// Urgency travels up the dependency edge. A ticket holding started work is
/// exactly as urgent as the thing it holds: otherwise the fleet works through
/// unrelated backlog while started work stands still. It is computed as a
/// closure down `deps` from everything in the `in_progress` group, not as a
/// single step: the immediate blocker may itself be blocked, and then the
/// candidate is the blocker's blocker, which one step does not reach.
///
/// The lift does not invert the priority scale: a `high` from the backlog still
/// beats a `low` blocker of a started `low`, because ranks are compared, not
/// the "unblocks something" flag. That flag breaks ties among equals.
const CTES: &str = r#"with recursive
               -- Начатое — это ГРУППА, а не статус с таким именем: в группе
               -- in_progress лежат и `in_progress`, и `to_test`. По литералу
               -- тикет на тестировании перестал бы считаться начатым, и его
               -- блокеры молча ушли бы в хвост очереди.
               started as (
                 select t.id, (select rank from priorities p where p.name = t.priority) as rank
                   from tickets t
                   join statuses s on s.name = t.status
                  where s.grp = 'in_progress' and t.deleted_at is null
               ),
               -- Вниз по рёбрам: что держит начатое, что держит того, кто
               -- держит начатое, и так далее.
               --
               -- Путь копится и проверяется явно, потому что цикл в данных
               -- возможен: схема запрещает только петлю на себя,
               -- `check (ticket_id <> depends_on)`, а кольцо из трёх тикетов
               -- ничем не запрещено. Без этой проверки обход не закончится.
               unblocks (id, rank, path) as (
                 select d.depends_on, st.rank, array[st.id, d.depends_on]
                   from started st
                   join deps d on d.ticket_id = st.id
                 union all
                 select d.depends_on, u.rank, u.path || d.depends_on
                   from unblocks u
                   join deps d on d.ticket_id = u.id
                  where not (d.depends_on = any(u.path))
               ),
               -- Самое срочное из того, что этот тикет разблокирует.
               needed as (
                 select id, min(rank) as rank from unblocks group by id
               )
"#;

/// The shared part of the selection: where from and what counts as eligible.
/// One for both forms — the claim and the preview must pick THE SAME ticket, or
/// a dry run would show something other than what gets taken.
const BODY: &str = r#"
                      join statuses s on s.name = c.status
                      left join needed n on n.id = c.id
                     where s.grp = 'todo' and c.status = 'open' and c.deleted_at is null
                       -- Заблокированный тикет не выдаётся вовсе.
                       --
                       -- Раньше зависимости были только справкой: их можно было
                       -- записать и посмотреть, но ни одна команда на них не
                       -- опиралась. Агент брал тикет, который физически нельзя
                       -- сделать, тратил на него подход и упирался — молча,
                       -- потому что отказа не было.
                       --
                       -- Разрешённой считается зависимость в терминальной
                       -- группе. Удалённый тикет не блокирует: его уже нет.
                       and not exists (
                             select 1 from deps d
                               join tickets dep on dep.id = d.depends_on
                               join statuses ds on ds.name = dep.status
                              where d.ticket_id = c.id
                                and dep.deleted_at is null
                                and ds.grp <> 'complete')"#;

const ORDER: &str = r#"
                     order by
                       -- 1. Эффективный ранг: свой собственный или того, что
                       -- этот тикет разблокирует, смотря что срочнее.
                       --
                       -- Он ПЕРВЫЙ намеренно. Шкала приоритетов не
                       -- переворачивается: признак «разблокирует» идёт вторым,
                       -- а не первым, поэтому high из бэклога по-прежнему
                       -- обгоняет low-блокер начатого low. Иначе одна забытая
                       -- зависимость у низкоприоритетного тикета вытаскивала бы
                       -- его вперёд всей очереди.
                       least(
                         coalesce((select rank from priorities p where p.name = c.priority), 2147483647),
                         coalesce(n.rank, 2147483647)),
                       -- 2. Снимает ли этот тикет блокер с начатого.
                       --
                       -- ВЫШЕ пожеланий, и это главное в порядке: тикет,
                       -- который держит начатую работу, не может уступать место
                       -- тикету, который просто удачно помечен. Раньше prefer
                       -- стоял первым и перекрывал подъём срочности — блокер
                       -- начатого без нужного тега уходил в самый конец, сколько
                       -- бы срочного он ни держал. Поймано на живой очереди:
                       -- два блокера начатого high не выдавались, потому что не
                       -- несли тега полосы.
                       case when n.id is null then 1 else 0 end,
                       -- 3. Позиция первого совпавшего тега в списке
                       -- предпочтений. Нет совпадений — в конец, но не выбывает:
                       -- prefer остаётся ПОЖЕЛАНИЕМ и ничего не отсекает. Чтобы
                       -- отсекать, есть отбор (Pick).
                       coalesce((select min(o.idx)
                                   from unnest($1::text[]) with ordinality as o(tag, idx)
                                  where o.tag = any(c.tags)), 2147483647),
                       c.created_at"#;

/// The tail of the claim: locking the row and returning what was taken.
const CLAIM_TAIL: &str = r#"
                     limit 1
                       -- OF c обязательно: без него FOR UPDATE запирает строки
                       -- ВСЕХ таблиц джойна, включая справочник statuses. Там
                       -- строка «open» одна на всех, и параллельные захваты
                       -- дерутся за неё, а SKIP LOCKED её пропускает. Замерено:
                       -- без OF c тикет достаётся 2 агентам из 40, с ним — 39.
                       -- Ровно то, ради чего затевался переезд, и ломалось молча:
                       -- агент видел «свободных нет» при полной очереди.
                       for update of c skip locked)
          returning t.id, t.title, t.status"#;

/// Filters for the claim. Deliberately separate from `prefer`: prefer sets an
/// ORDER and excludes nothing, a filter EXCLUDES. Merging them would lose
/// "infrastructure first, and if there is none, give me anything".
///
/// There is no status here and there will not be: next takes only `open` and
/// only with closed dependencies. Allowing an arbitrary status would permit
/// claiming something started or blocked — precisely what the guard exists
/// for.
#[derive(Default)]
pub struct Pick {
    pub tags: Vec<String>,
    /// The tag must match in full rather than as a part.
    pub strict: bool,
    pub project: Option<String>,
    pub module: Option<String>,
    /// Any live module instead of a named one.
    ///
    /// A dispatcher needs "the ticket has a unit of work assigned at all",
    /// not a list of two hundred names. An archived module does not count: work
    /// on it has stopped, and there is no point handing such a ticket out.
    pub has_module: bool,
    pub assignee: Option<String>,
    /// Show what WOULD be taken without taking anything.
    ///
    /// Same filter, same order, same ticket — the only difference is that
    /// nothing is written. Needed by a dispatcher checking its own selection,
    /// and by a reviewer who should not steal a ticket from the fleet just to
    /// look at it.
    ///
    /// The answer here is ADVICE, not a reservation: two callers in a row will
    /// see the same ticket, and by the time it is claimed it may be gone. So
    /// the row is not locked (`for update` would be harmful here: it would hold
    /// up other people's claims), and the client says out loud that the ticket
    /// was not taken.
    pub dry_run: bool,
}

pub async fn next(
    tx: &Transaction<'_>,
    prefer: &[String],
    f: &Pick,
) -> anyhow::Result<Option<Claimed>> {
    let mut args: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![&prefer];
    let mut cond = String::new();
    if let Some(v) = &f.project {
        args.push(v);
        cond.push_str(&format!(" and c.project_id = ${}", args.len()));
    }
    if let Some(v) = &f.module {
        args.push(v);
        cond.push_str(&format!(" and c.module = ${}", args.len()));
    }
    if f.has_module {
        cond.push_str(
            " and c.module is not null
               and not exists (select 1 from modules m
                                where m.project_id = c.project_id
                                  and m.name = c.module
                                  and m.archived_at is not null)",
        );
    }
    if let Some(v) = &f.assignee {
        args.push(v);
        cond.push_str(&format!(" and c.assignee = ${}", args.len()));
    }
    if !f.tags.is_empty() {
        if f.strict {
            args.push(&f.tags);
            // An array with `@>` takes the GIN index; `= any(...)` goes to a scan.
            cond.push_str(&format!(" and c.tags @> ${}::text[]", args.len()));
        } else {
            // Every named tag must occur in at least one of the ticket's tags:
            // AND between the named ones, substring within each — as in the list.
            for t in &f.tags {
                args.push(t);
                cond.push_str(&format!(
                    " and exists (select 1 from unnest(c.tags) x where x ilike '%' || ${} || '%')",
                    args.len()
                ));
            }
        }
    }

    // Both forms are assembled from THE SAME parts: the filter and the order
    // are shared, only the head and the tail differ. Otherwise the preview
    // would one day show a different ticket from the one that gets taken, and
    // it could not be trusted.
    let sql = if f.dry_run {
        format!("{CTES} select c.id, c.title, c.status from tickets c {BODY}{cond}{ORDER}\n limit 1")
    } else {
        format!(
            "{CTES}            update tickets t
                set status = 'in_progress'
              where t.id = (
                    select c.id from tickets c {BODY}{cond}{ORDER}{CLAIM_TAIL}"
        )
    };
    let rows = tx.query(&sql, &args).await?;
    Ok(rows.first().map(|r| Claimed {
        id: r.get(0),
        title: r.get(1),
        status: r.get(2),
    }))
}


/// `start <id>` — take a NAMED ticket.
///
/// `SKIP LOCKED` does not fit here: skipping a locked row would mean "no such
/// ticket", whereas the truth is that it exists and is already taken. A
/// conditional write returns zero rows, and that is exactly what needs saying
/// out loud.
pub enum StartOutcome {
    Taken(Claimed),
    AlreadyTaken { status: String, agent: Option<String> },
    NoSuchTicket,
    /// The ticket stands on unclosed ones. There is deliberately no override:
    /// blocked means blocked, otherwise a "do it anyway" flag becomes a habit
    /// and dependencies stop meaning anything.
    Blocked { deps: Vec<(String, String)> },
}

pub async fn start(tx: &Transaction<'_>, id: &str) -> anyhow::Result<StartOutcome> {
    // Dependencies first, then the claim. The order matters: claiming and
    // rolling back shows the ticket as taken for a moment, and a neighbouring
    // agent will see it occupied in that window and leave empty-handed.
    let blockers = tx
        .query(
            "select dep.id, dep.status from deps d
               join tickets dep on dep.id = d.depends_on
               join statuses ds on ds.name = dep.status
              where d.ticket_id = $1
                and dep.deleted_at is null
                and ds.grp <> 'complete'
              order by dep.id",
            &[&id],
        )
        .await?;
    if !blockers.is_empty() {
        return Ok(StartOutcome::Blocked {
            deps: blockers.iter().map(|r| (r.get(0), r.get(1))).collect(),
        });
    }

    let rows = tx
        .query(
            "update tickets set status = 'in_progress'
              where id = $1 and status = 'open'
          returning id, title, status",
            &[&id],
        )
        .await?;
    if let Some(r) = rows.first() {
        return Ok(StartOutcome::Taken(Claimed {
            id: r.get(0),
            title: r.get(1),
            status: r.get(2),
        }));
    }
    // Zero rows means two different things, and they must not be conflated:
    // "no such ticket" and "already taken" call for different behaviour from
    // the caller.
    match tx
        .query_opt("select status, assignee from tickets where id = $1 and deleted_at is null", &[&id])
        .await?
    {
        Some(r) => Ok(StartOutcome::AlreadyTaken {
            status: r.get(0),
            agent: r.get(1),
        }),
        None => Ok(StartOutcome::NoSuchTicket),
    }
}

/// The "already picked up" guard: only what is in the `todo` group may be
/// edited freely. The policy lives in a table rather than in code — the owner
/// changes the behaviour with one UPDATE, without a release.
pub async fn requires_force(tx: &Transaction<'_>, status: &str) -> anyhow::Result<bool> {
    Ok(tx
        .query_opt(
            "select p.requires_force
               from statuses s join status_policy p on p.grp = s.grp
              where s.name = $1",
            &[&status],
        )
        .await?
        .map(|r| r.get(0))
        .unwrap_or(false))
}

#[cfg(test)]
mod pick_tests {
    use super::Pick;

    // An empty filter must add no conditions at all: otherwise `ntk next` with
    // no flags would start excluding what it used to hand out.
    #[test]
    fn an_empty_pick_narrows_nothing() {
        let p = Pick::default();
        assert!(p.tags.is_empty());
        assert!(!p.strict && !p.has_module);
        assert!(p.project.is_none() && p.module.is_none() && p.assignee.is_none());
    }

    // The difference the filter exists separately from prefer for: prefer sets
    // an order and excludes nothing, a filter excludes. Merging them loses
    // "infrastructure first, and if there is none, give me anything".
    #[test]
    fn a_pick_is_not_a_preference() {
        let p = Pick { tags: vec!["infra".into()], ..Default::default() };
        assert_eq!(p.tags, vec!["infra".to_string()]);
        // prefer is not in Pick and must not be — it arrives as its own argument.
    }
}
