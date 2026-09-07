//! Импорт снимка Notion в Postgres и сверка результата.
//!
//! Разовая административная задача: пишет в базу напрямую на дроплете, не
//! через API. Идемпотентна по ntk-id — повторный прогон не создаёт дублей и
//! не теряет уже перенесённое.
//!
//! Сверка здесь не формальность. Количества строк совпадут и на потерянных
//! телах: 365 записей на месте, а внутри дыры. Поэтому сверяются ещё теги,
//! рёбра зависимостей и контрольные суммы тел.

mod rows;

use anyhow::{Context, Result, bail};
use rows::*;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use tokio_postgres::NoTls;

struct Row {
    id: String,
    title: String,
    status: String,
    priority: Option<String>,
    kind: Option<String>,
    assignee: Option<String>,
    project: Option<String>,
    tags: Vec<String>,
    body: String,
    due: Option<String>,
    created_at: String,
    closed_at: Option<String>,
    deps_uuid: Vec<String>,
    page_uuid: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let snapshot = args.next().context("нужен путь к снимку: ntk-import <snapshot.jsonl> <schema> [--verify-only]")?;
    let schema = args.next().context("нужна схема воркспейса: ntk-import <snapshot.jsonl> <schema> [--verify-only]")?;
    // Сверка сразу после записи проверяет то, что сама же и записала. Настоящую
    // проверку даёт отдельный прогон: снимок против базы, ничего не трогая.
    // Поймано мутацией — удалённая строка при обычном прогоне возвращалась
    // импортом, и сверка её пропажи не замечала.
    let verify_only = args.any(|a| a == "--verify-only");
    if !schema.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        bail!("недопустимое имя схемы: {schema:?}");
    }

    let raw = std::fs::read_to_string(&snapshot).with_context(|| format!("не читается {snapshot}"))?;
    let people = people_map()?;
    let mut rows = Vec::new();
    let mut unknown_priority: BTreeSet<String> = BTreeSet::new();
    let mut unknown_people: BTreeSet<String> = BTreeSet::new();
    let mut incomplete: Vec<String> = Vec::new();

    for line in raw.lines().filter(|l| !l.trim().is_empty()) {
        let rec: Value = serde_json::from_str(line).context("строка снимка не разбирается")?;
        let page = &rec["page"];
        let props = &page["properties"];

        let Some(id) = text_prop(props, "Ticket ID") else { continue };
        let raw_priority = select_prop(props, "Priority");
        let priority = match raw_priority.as_deref() {
            None => None,
            Some(p) => match canonical_priority(Some(p)) {
                Some(c) => Some(c.to_string()),
                None => {
                    unknown_priority.insert(p.to_string());
                    None
                }
            },
        };
        let assignee = match people_ids(props, "Assignee").first() {
            None => None,
            Some(uuid) => match people.get(uuid) {
                Some(handle) => Some(handle.clone()),
                None => {
                    unknown_people.insert(uuid.clone());
                    None
                }
            },
        };
        if rec["body_complete"].as_bool() == Some(false) {
            incomplete.push(id.clone());
        }

        rows.push(Row {
            id,
            title: text_prop(props, "Name").unwrap_or_default(),
            status: status_prop(props, "Status").unwrap_or_else(|| "open".into()),
            priority,
            kind: select_prop(props, "Type"),
            assignee,
            project: select_prop(props, "Project"),
            tags: multi_select(props, "Tags"),
            body: rec["body"].as_str().unwrap_or_default().to_string(),
            due: date_prop(props, "Due"),
            created_at: page["created_time"].as_str().unwrap_or_default().to_string(),
            closed_at: date_prop(props, "Closed At"),
            deps_uuid: relation_ids(props, "Deps"),
            page_uuid: page["id"].as_str().unwrap_or_default().to_string(),
        });
    }

    // Отказ до записи, а не после: перенос с молча потерянным приоритетом или
    // исполнителем выглядит успешным ровно до того дня, когда кто-то заметит.
    if !incomplete.is_empty() {
        bail!(
            "в снимке {} тел помечены как недочитанные (body_complete=false): {}\n\
             импортировать их значит перенести дыру и не узнать об этом",
            incomplete.len(),
            incomplete.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
        );
    }
    if !unknown_priority.is_empty() {
        bail!("незнакомые значения приоритета: {:?}\nдобавьте их в canonical_priority — молча ронять важность нельзя",
              unknown_priority);
    }

    println!("разобрано строк: {}", rows.len());
    if !unknown_people.is_empty() {
        println!("исполнители без записи в конфиге ({}): {:?} — у их тикетов assignee останется пустым",
                 unknown_people.len(), unknown_people);
    }

    let url = std::env::var("DATABASE_URL")
        .context("DATABASE_URL не задан — строка администратора лежит в /etc/ntk/admin.env")?;
    let (client, connection) = tokio_postgres::connect(&url, NoTls).await.context("нет соединения с базой")?;
    tokio::spawn(async move {
        if let Err(e) = connection.await { eprintln!("соединение разорвано: {e}"); }
    });
    client.batch_execute(&format!("set search_path to {schema}, core, public")).await?;

    // Исполнители обязаны существовать в core.users: FK не даст вставить
    // выдуманного, и лучше узнать об этом списком здесь, чем построчно потом.
    let needed: BTreeSet<&str> = rows.iter().filter_map(|r| r.assignee.as_deref()).collect();
    let known: BTreeSet<String> = client
        .query("select id from core.users", &[])
        .await?
        .iter()
        .map(|r| r.get::<_, String>(0))
        .collect();
    let missing: Vec<&&str> = needed.iter().filter(|h| !known.contains(**h)).collect();
    if !missing.is_empty() {
        bail!("в core.users нет исполнителей: {missing:?}\nих заводит администратор — импорт их не выдумывает");
    }

    if verify_only {
        println!("сверка без записи: {} строк снимка против схемы {schema}", rows.len());
    } else {
        import(&client, &rows).await?;
    }
    verify(&client, &rows).await
}

async fn import(client: &tokio_postgres::Client, rows: &[Row]) -> Result<()> {
    let projects: BTreeSet<&str> = rows.iter().filter_map(|r| r.project.as_deref()).collect();
    for p in &projects {
        client
            .execute("insert into projects (id, name) values ($1, $1) on conflict (id) do nothing", &[p])
            .await?;
    }

    let by_uuid: HashMap<&str, &str> =
        rows.iter().map(|r| (r.page_uuid.as_str(), r.id.as_str())).collect();

    for r in rows {
        client.execute(
            "insert into tickets (id, title, status, priority, type, assignee, project_id, tags, body, due, created_at, closed_at)
             values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10::text::date,$11::text::timestamptz,$12::text::timestamptz)
             on conflict (id) do update set
               title = excluded.title, status = excluded.status, priority = excluded.priority,
               type = excluded.type, assignee = excluded.assignee, project_id = excluded.project_id,
               tags = excluded.tags, body = excluded.body, due = excluded.due",
            &[&r.id, &r.title, &r.status, &r.priority, &r.kind, &r.assignee, &r.project,
              &r.tags, &r.body, &r.due, &r.created_at, &r.closed_at],
        ).await.with_context(|| format!("не удалось записать {}", r.id))?;
    }

    // Рёбра ставятся после всех строк: зависимость может указывать на тикет,
    // который в снимке идёт позже.
    let mut edges = 0usize;
    let mut dangling = 0usize;
    for r in rows {
        for uuid in &r.deps_uuid {
            match by_uuid.get(uuid.as_str()) {
                Some(target) => {
                    client.execute(
                        "insert into deps (ticket_id, depends_on) values ($1,$2) on conflict do nothing",
                        &[&r.id, target],
                    ).await?;
                    edges += 1;
                }
                // Зависимость на тикет вне этой базы: в Notion такое есть, и
                // молча превращать её в «зависимостей нет» нельзя.
                None => dangling += 1,
            }
        }
    }
    println!("перенесено: {} тикетов, {} проектов, {} рёбер", rows.len(), projects.len(), edges);
    if dangling > 0 {
        println!("зависимостей за пределами этой базы: {dangling} — они не перенесены, потому что цели здесь нет");
    }
    Ok(())
}

/// Сверка снимка с базой. Ненулевой код при любом расхождении.
async fn verify(client: &tokio_postgres::Client, rows: &[Row]) -> Result<()> {
    let mut problems: Vec<String> = Vec::new();

    let mut expected_by_status: BTreeMap<&str, usize> = BTreeMap::new();
    for r in rows {
        *expected_by_status.entry(r.status.as_str()).or_default() += 1;
    }
    for (status, want) in &expected_by_status {
        let got: i64 = client
            .query_one("select count(*) from tickets where status = $1", &[status])
            .await?
            .get(0);
        if got as usize != *want {
            problems.push(format!("{status}: в снимке {want}, в базе {got}"));
        }
    }

    let total: i64 = client.query_one("select count(*) from tickets", &[]).await?.get(0);
    if total as usize != rows.len() {
        problems.push(format!("всего: в снимке {}, в базе {total}", rows.len()));
    }

    // Тела сверяются суммой, а не длиной: строка той же длины с другим
    // содержимым — ровно то, что сверка длин пропустит.
    let mut body_mismatch = 0usize;
    for r in rows {
        let got: Option<String> = client
            .query_opt("select body from tickets where id = $1", &[&r.id])
            .await?
            .map(|row| row.get(0));
        match got {
            Some(b) if digest(&b) == digest(&r.body) => {}
            Some(_) => body_mismatch += 1,
            None => problems.push(format!("{}: строки нет в базе", r.id)),
        }
    }
    if body_mismatch > 0 {
        problems.push(format!("тела разошлись у {body_mismatch} тикетов"));
    }

    // Даты закрытия сверяются отдельно, потому что мимо всего остального они
    // проходят незамеченными: количества сходятся, тела целы, а дата закрытия
    // выдумана. Так и случилось — триггер ставил сегодняшний день всем 1603
    // закрытым тикетам, включая 436, у которых настоящая дата была известна.
    let mut closed_mismatch: Vec<String> = Vec::new();
    for r in rows {
        let got: Option<Option<String>> = client
            .query_opt("select to_char(closed_at, 'YYYY-MM-DD') from tickets where id = $1", &[&r.id])
            .await?
            .map(|row| row.get(0));
        let want = r.closed_at.as_ref().map(|d| d.chars().take(10).collect::<String>());
        if let Some(got) = got {
            if got != want {
                closed_mismatch.push(format!("{}: снимок {:?}, база {:?}", r.id, want, got));
            }
        }
    }
    if !closed_mismatch.is_empty() {
        problems.push(format!(
            "даты закрытия разошлись у {} тикетов, например: {}",
            closed_mismatch.len(),
            closed_mismatch.iter().take(3).cloned().collect::<Vec<_>>().join("; ")
        ));
    }

    // Поля, которые база ВЫЧИСЛЯЕТ, а не переносит, сверяются поимённо —
    // правило из ntk-zxksmdiiat. Оно появилось после того, как зелёная сверка
    // сопровождала 1603 выдуманные даты закрытия: количества сходились, теги
    // были на месте, суммы тел совпадали, потому что тела целы.
    //
    // Здесь проверяется не равенство снимку (в Notion этих полей нет), а то,
    // что база их не ВЫДУМАЛА: у исторической строки, привезённой импортом,
    // не может быть отметки о начале работы, поставленной сегодня.
    let today: String = client
        .query_one("select to_char(now(), 'YYYY-MM-DD')", &[])
        .await?
        .get(0);

    let invented_start: i64 = client
        .query_one(
            "select count(*) from tickets
              where started_at is not null and to_char(started_at, 'YYYY-MM-DD') = $1",
            &[&today],
        )
        .await?
        .get(0);
    if invented_start > 0 {
        problems.push(format!(
            "{invented_start} тикетов получили started_at сегодняшним днём — импорт не переносит эту отметку, значит она выдумана триггером"
        ));
    }

    let invented_closed: i64 = client
        .query_one(
            "select count(*) from tickets t
              where t.closed_at is not null
                and to_char(t.closed_at, 'YYYY-MM-DD') = $1
                and t.id <> all($2::text[])",
            &[
                &today,
                &rows.iter()
                    .filter(|r| r.closed_at.as_deref().map(|d| d.starts_with(&today)).unwrap_or(false))
                    .map(|r| r.id.clone())
                    .collect::<Vec<_>>(),
            ],
        )
        .await?
        .get(0);
    if invented_closed > 0 {
        problems.push(format!(
            "{invented_closed} тикетов закрыты сегодняшним днём, которого нет в снимке — дата выдумана"
        ));
    }

    // current_status_at вычисляется базой, значит подлежит сверке — то же
    // правило. Равенства снимку тут быть не может: истории переходов в Notion
    // нет, и для привезённых строк значение заведомо приближённое. Проверяется
    // то, что оно не ЛОЖНО: отметка не может быть раньше создания тикета и не
    // может относиться к будущему.
    let impossible: i64 = client
        .query_one(
            // Отметка, РАВНАЯ дате закрытия, пришла из данных, а не вычислена:
            // в Notion встречается Closed At на минуты раньше created_time, и
            // ругаться на это значит ругаться на правду. Проверяется то, что
            // база могла выдумать сама.
            "select count(*) from tickets
              where current_status_at is not null
                and current_status_at is distinct from closed_at
                and (current_status_at < created_at or current_status_at > now())",
            &[],
        )
        .await?
        .get(0);
    if impossible > 0 {
        problems.push(format!(
            "{impossible} тикетов имеют current_status_at раньше создания или в будущем"
        ));
    }

    // И отдельно: у закрытых она обязана совпадать с датой закрытия. Разошлись
    // — значит статус меняли, а отметка осталась от прошлого состояния, то
    // есть «сколько висит в этом статусе» отвечает неправду.
    let stale_closed: i64 = client
        .query_one(
            "select count(*) from tickets t
               join statuses s on s.name = t.status
              where s.grp = 'complete' and t.closed_at is not null
                and t.current_status_at is distinct from t.closed_at",
            &[],
        )
        .await?
        .get(0);
    if stale_closed > 0 {
        problems.push(format!(
            "{stale_closed} закрытых тикетов: отметка текущего статуса не совпадает с датой закрытия"
        ));
    }

    let expected_tags: BTreeSet<&str> = rows.iter().flat_map(|r| r.tags.iter().map(String::as_str)).collect();
    let got_tags: BTreeSet<String> = client
        .query("select distinct unnest(tags) from tickets", &[])
        .await?
        .iter()
        .map(|r| r.get::<_, String>(0))
        .collect();
    for t in &expected_tags {
        if !got_tags.contains(*t) {
            problems.push(format!("тег потерян: {t}"));
        }
    }

    if problems.is_empty() {
        println!("сверка пройдена: {} тикетов, {} тегов, тела совпадают по контрольным суммам",
                 rows.len(), expected_tags.len());
        Ok(())
    } else {
        for p in &problems {
            eprintln!("расхождение: {p}");
        }
        bail!("сверка не пройдена: {} расхождений", problems.len())
    }
}

fn digest(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// Notion знает людей по uuid, а core.users — по короткому хэндлу. Карта
/// берётся из конфига CLI, где она уже есть; выдумывать соответствие нельзя.
fn people_map() -> Result<HashMap<String, String>> {
    let home = std::env::var("HOME").context("HOME не задан")?;
    let path = std::path::Path::new(&home).join(".config/ntk/config.json");
    let raw = std::fs::read_to_string(&path).with_context(|| format!("не читается {}", path.display()))?;
    let cfg: Value = serde_json::from_str(&raw)?;
    let mut out = HashMap::new();
    if let Some(workspaces) = cfg.get("workspaces").and_then(Value::as_object) {
        for ws in workspaces.values() {
            if let Some(ids) = ws.get("assignee_ids").and_then(Value::as_object) {
                for (handle, uuid) in ids {
                    if let Some(u) = uuid.as_str() {
                        out.insert(u.to_string(), handle.clone());
                    }
                }
            }
        }
    }
    Ok(out)
}
