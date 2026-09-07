//! Захват тикета — то, ради чего затевался переезд.
//!
//! В Notion между чтением статуса и записью было окно: два агента видели один
//! `open` и оба писали `in_progress`. Условной записи Notion не давал, обойти
//! это было нечем. Здесь захват атомарен, и механизма два, потому что задачи
//! разные.

use tokio_postgres::Transaction;

pub struct Claimed {
    pub id: String,
    pub title: String,
    pub status: String,
}

/// `next` — взять любой свободный, при желании предпочитая теги.
///
/// ВАЖНО: захват НЕ трогает `assignee`. Это поле отвечает на вопрос «кто
/// отвечает за тикет», а не «кто его сейчас держит»; переписывать его при
/// взятии в работу означало бы молча стирать ответственного. Взятие меняет
/// только статус — переход в `in_progress` и есть факт взятия.
///
/// `FOR UPDATE SKIP LOCKED`: параллельные агенты разбирают очередь, не
/// блокируя друг друга. Тот, кому строка досталась, работает; остальные видят
/// следующую, а не ждут.
///
/// `prefer` задаёт порядок предпочтения: `["infra","alpha"]` означает
/// «сначала всё из infra, потом alpha, потом остальное». Не фильтр, а
/// порядок — иначе агент простаивал бы при пустой полосе вместо того, чтобы
/// взять следующее по важности.
///
/// Срочность поднимается по ребру зависимости. Тикет, который держит начатую
/// работу, срочен ровно настолько, насколько срочно то, что он держит: иначе
/// флот разбирает посторонний бэклог, пока начатое стоит. Считается это
/// замыканием вниз по `deps` от всего, что в группе `in_progress`, а не одним
/// шагом: непосредственный блокер сам может быть заблокирован, и тогда
/// кандидатом окажется блокер блокера, до которого один шаг не достаёт.
///
/// Шкалу приоритетов подъём не переворачивает: `high` из бэклога по-прежнему
/// обгоняет `low`-блокер начатого `low`, потому что сравниваются ранги, а не
/// признак «разблокирует». Признак работает тай-брейком среди равных.
pub async fn next(tx: &Transaction<'_>, prefer: &[String]) -> anyhow::Result<Option<Claimed>> {
    let rows = tx
        .query(
            "with recursive
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
            update tickets t
                set status = 'in_progress'
              where t.id = (
                    select c.id from tickets c
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
                                and ds.grp <> 'complete')
                     order by
                       -- Позиция первого совпавшего тега в списке предпочтений.
                       -- Нет совпадений — в конец, но не выбывает.
                       coalesce((select min(o.idx)
                                   from unnest($1::text[]) with ordinality as o(tag, idx)
                                  where o.tag = any(c.tags)), 2147483647),
                       -- Эффективный ранг: свой собственный или того, что этот
                       -- тикет разблокирует, смотря что срочнее.
                       least(
                         coalesce((select rank from priorities p where p.name = c.priority), 2147483647),
                         coalesce(n.rank, 2147483647)),
                       -- Среди равных по срочности первым идёт тот, кто снимает
                       -- блокер с начатого: работа, в которую уже вложились,
                       -- дороже ещё не начатой.
                       case when n.id is null then 1 else 0 end,
                       c.created_at
                     limit 1
                       -- OF c обязательно: без него FOR UPDATE запирает строки
                       -- ВСЕХ таблиц джойна, включая справочник statuses. Там
                       -- строка «open» одна на всех, и параллельные захваты
                       -- дерутся за неё, а SKIP LOCKED её пропускает. Замерено:
                       -- без OF c тикет достаётся 2 агентам из 40, с ним — 39.
                       -- Ровно то, ради чего затевался переезд, и ломалось молча:
                       -- агент видел «свободных нет» при полной очереди.
                       for update of c skip locked)
          returning t.id, t.title, t.status",
            &[&prefer],
        )
        .await?;
    Ok(rows.first().map(|r| Claimed {
        id: r.get(0),
        title: r.get(1),
        status: r.get(2),
    }))
}

/// `start <id>` — взять КОНКРЕТНЫЙ тикет.
///
/// Здесь `SKIP LOCKED` не подходит: пропустить занятую строку означало бы
/// «тикета нет», а правда в том, что он есть и уже занят. Условная запись
/// возвращает ноль строк, и это ровно то, что нужно сказать наружу.
pub enum StartOutcome {
    Taken(Claimed),
    AlreadyTaken { status: String, agent: Option<String> },
    NoSuchTicket,
    /// Тикет стоит на незакрытых. Обхода нет намеренно: заблокирован — значит
    /// заблокирован, иначе флаг «сделать всё равно» становится привычкой и
    /// зависимости перестают что-либо значить.
    Blocked { deps: Vec<(String, String)> },
}

pub async fn start(tx: &Transaction<'_>, id: &str) -> anyhow::Result<StartOutcome> {
    // Сначала зависимости, потом захват. Порядок именно такой: захватить и
    // откатить значит на мгновение показать тикет взятым, а соседний агент за
    // это время увидит его занятым и уйдёт ни с чем.
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
    // Ноль строк значит две разные вещи, и путать их нельзя: «нет тикета» и
    // «уже взят» требуют от вызывающего разного поведения.
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

/// Гард «тикет уже подобран»: править свободно можно только то, что в группе
/// `todo`. Политика лежит в таблице, а не в коде — владелец меняет поведение
/// одним UPDATE, без релиза.
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
