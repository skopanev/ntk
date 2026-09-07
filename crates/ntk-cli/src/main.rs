//! ntk — клиент. Одна команда логина и чтение тикетов.

mod api;
mod config;
mod mcp;
mod upgrade;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ntk", about = "Тикеты: очередь агентов и людей", version)]
struct Cli {
    /// Воркспейс. Можно и до команды, и после: в старом инструменте флаг был
    /// общим, и люди набирают его по привычке впереди.
    #[arg(short = 'W', long, global = true)]
    workspace: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Вход через браузер. Ключ не вводится руками и не приходит по почте.
    Login,
    /// Список тикетов. По умолчанию — только свои.
    Ls {
        /// Воркспейс. Без него берётся из .ntkrc; догадок нет.
        #[arg(short = 's', long)]
        status: Option<String>,
        /// Отбор по тегам через запятую. Все перечисленные должны быть на
        /// тикете: «и», а не «или». По умолчанию по вхождению: "infra"
        /// находит и "initiative:infra".
        #[arg(short = 't', long)]
        tag: Option<String>,
        /// Тег должен совпасть целиком, а не войти частью.
        #[arg(long)]
        strict: bool,
        /// Отбор по исполнителю. Сильнее умолчания «мои».
        #[arg(short = 'a', long)]
        assignee: Option<String>,
        /// Отбор по проекту.
        #[arg(short = 'P', long)]
        project: Option<String>,
        /// Отбор по модулю.
        #[arg(long)]
        module: Option<String>,
        /// Отбор по заголовку: вхождение подстроки, регистр не важен.
        #[arg(short = 'q', long)]
        title: Option<String>,
        /// Только число подходящих, без самих тикетов. Считает база, поэтому
        /// потолок в 500 на выдачу счёту не мешает.
        #[arg(long)]
        count: bool,
        #[arg(short = 'n', long, default_value_t = 50)]
        limit: i64,
        /// Пропустить первые N — следующая страница.
        #[arg(short = 'o', long, default_value_t = 0)]
        offset: i64,
        /// Показать тикеты всех, а не только свои.
        #[arg(long)]
        all: bool,
        /// Вывести JSON вместо таблицы.
        #[arg(long)]
        json: bool,
    },
    /// Пройти тикеты по одному для проверки. Ничего не меняет.
    ///
    /// В отличие от next, который берёт тикет В РАБОТУ: пройти так тридцать
    /// тикетов значит перевести их все на себя, то есть испортить очередь.
    Walk {
        /// Отбор по модулю.
        #[arg(long)]
        module: Option<String>,
        #[arg(short = 's', long)]
        status: Option<String>,
        /// Теги через запятую. По умолчанию по вхождению.
        #[arg(short = 't', long)]
        tag: Option<String>,
        #[arg(long)]
        strict: bool,
        /// Отбор по заголовку.
        #[arg(short = 'q', long)]
        title: Option<String>,
        #[arg(short = 'a', long)]
        assignee: Option<String>,
        #[arg(short = 'P', long)]
        project: Option<String>,
        /// Все, а не только свои.
        #[arg(long)]
        all: bool,
        /// Забыть показанное и пойти сначала.
        #[arg(long)]
        reset: bool,
        #[arg(long)]
        json: bool,
    },
    /// Показать тикет целиком: поля, тело, зависимости.
    Show {
        /// Идентификатор. Регистр не важен.
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Кто я и какие воркспейсы доступны.
    Whoami,
    /// Обновиться до последней версии.
    ///
    /// Скачанное проверяется контрольной суммой И подписью, и только потом
    /// заменяет текущий бинарь.
    Upgrade,
    /// Отдавать те же команды по MCP — для Claude Desktop и агентов.
    ///
    /// Говорит по stdio: клиент запускает бинарь и общается с ним через
    /// стандартный ввод-вывод, порта и установки не требуется.
    Mcp,
    /// Завести тикет.
    Create {
        /// Заголовок.
        title: String,
        /// Проект. Он же префикс идентификатора.
        #[arg(short = 'P', long)]
        project: Option<String>,
        #[arg(short = 'p', long)]
        priority: Option<String>,
        #[arg(short = 'a', long)]
        assignee: Option<String>,
        #[arg(short = 'T', long = "type")]
        kind: Option<String>,
        #[arg(short = 's', long)]
        status: Option<String>,
        /// Теги через запятую.
        #[arg(short = 't', long)]
        tags: Option<String>,
        /// Тело тикета.
        #[arg(short = 'b', long, short_alias = 'd')]
        body: Option<String>,
        /// Идентификаторы тикетов, которых этот ждёт, через запятую.
        #[arg(long)]
        deps: Option<String>,
        /// Модуль — единица работы внутри проекта. Допустимые перечисляет
        /// `ntk meta`: угадывать их не нужно и не следует.
        #[arg(long)]
        module: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Изменить тикет.
    Update {
        id: String,
        #[arg(short = 's', long)]
        status: Option<String>,
        #[arg(long)]
        title: Option<String>,
        /// Тело целиком. ЗАМЕНЯЕТ прежнее — чтобы дописать, нужен -A.
        #[arg(short = 'b', long, short_alias = 'd')]
        body: Option<String>,
        /// Дописать в конец тела, не трогая написанное.
        ///
        /// Был в старом инструменте и потерялся при переписывании. Без него
        /// единственный способ добавить строку — вычитать тело, склеить у себя
        /// и записать целиком через -b, а это и лишний повод стереть чужое, и
        /// гонка: два дописывания подряд, и одно пропадает молча.
        #[arg(short = 'A', long)]
        append: Option<String>,
        #[arg(short = 'a', long)]
        assignee: Option<String>,
        /// Правки тегов через запятую, каждый со знаком: "+alpha,-legacy".
        /// Знак обязателен — иначе «добавить» однажды окажется «заменить всё».
        /// allow_hyphen_values обязателен: "-тег" иначе выглядит для разбора
        /// как флаг, и снятие тега вообще невозможно набрать.
        #[arg(short = 't', long = "tags", visible_alias = "tag", allow_hyphen_values = true)]
        tags: Option<String>,
        /// Зависимости, три формы как в старом инструменте:
        /// "a,b" — ЗАМЕНИТЬ набор целиком, "+a,-b" — добавить и снять,
        /// "" — снять все. Смешивать формы нельзя: голый элемент рядом со
        /// знаком однажды значил «добавить», и на этой двусмысленности
        /// терялись связи.
        #[arg(long = "deps", visible_alias = "dep", allow_hyphen_values = true)]
        deps: Option<String>,
        /// Приоритет. Изменить его было НЕЛЬЗЯ вообще — а это первое, что
        /// правят, когда работа оказывается срочнее, чем думали.
        #[arg(short = 'p', long)]
        priority: Option<String>,
        #[arg(short = 'T', long = "type")]
        kind: Option<String>,
        #[arg(short = 'P', long)]
        project: Option<String>,
        /// Срок, YYYY-MM-DD. Пустая строка снимает его.
        #[arg(long)]
        due: Option<String>,
        /// Модуль. Пустая строка снимает его. При смене проекта модуль нового
        /// проекта обязателен: молча снять его нельзя.
        #[arg(long)]
        module: Option<String>,
        /// Менять тикет, который уже кем-то подобран.
        #[arg(long)]
        force: bool,
    },
    /// Взять конкретный тикет в работу.
    Start {
        id: String,
    },
    /// Зависимости тикета: на чём стоит и что стоит на нём.
    Deps {
        id: String,
        /// Только то, чего тикет ждёт. Без флагов показываются обе стороны.
        #[arg(long, conflicts_with = "down")]
        up: bool,
        /// Только те, кто ждёт этот тикет.
        #[arg(long)]
        down: bool,
        #[arg(long)]
        json: bool,
    },
    /// Убрать тикет: он перестаёт показываться, но не стирается.
    Rm {
        id: String,
        /// Не спрашивать подтверждения.
        #[arg(short = 'y', long)]
        yes: bool,
    },
    /// Модули проекта: действующие и архивные.
    Modules {
        /// Проект. Без него — модули всех проектов воркспейса.
        #[arg(short = 'P', long)]
        project: Option<String>,
        /// Заменить список модулей проекта тем, что придёт со стандартного
        /// ввода: по одному имени в строке. Список считается ПОЛНЫМ.
        #[arg(long)]
        replace: bool,
        /// Читать список со стандартного ввода. Пишется явно, чтобы замена
        /// набора никогда не случалась по недосмотру.
        #[arg(long)]
        stdin: bool,
        #[arg(long)]
        json: bool,
    },
    /// Что есть в воркспейсе: статусы, приоритеты, проекты, люди.
    Meta {
        #[arg(long)]
        json: bool,
    },
    /// Закрыть тикет: перевести в done.
    Close {
        id: String,
        /// Закрыть тикет, который уже кем-то подобран.
        #[arg(long)]
        force: bool,
    },
    /// Взять следующий свободный тикет в работу.
    ///
    /// --prefer задаёт ПОРЯДОК предпочтения тегов, а не фильтр: когда тикеты
    /// с первым тегом кончились, берётся следующий, и полоса не простаивает.
    Next {
        /// Теги в порядке предпочтения, через запятую. Не фильтр: если по ним
        /// ничего нет, будет взят любой свободный тикет.
        #[arg(long)]
        prefer: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let ws = cli.workspace.clone();
    match cli.cmd {
        Cmd::Login => login().await,
        Cmd::Ls { status, tag, strict, title, assignee, project, module, count, limit, offset, all, json } => {
            let f = api::Filters { status, tag, title, assignee, project, module, strict, all };
            ls(ws, f, count, limit, offset, json).await
        }
        Cmd::Walk { status, tag, strict, title, assignee, project, module, all, reset, json } => {
            let f = api::Filters { status, tag, title, assignee, project, module, strict, all };
            walk(ws, f, reset, json).await
        }
        Cmd::Show { id, json } => show(id, ws, json).await,
        Cmd::Next { prefer, json } => next(ws, prefer, json).await,
        Cmd::Create { title, project, priority, assignee, kind, status, tags, body, deps, module, json } =>
            create(title, ws, project, priority, assignee, kind, status, tags, body, deps, module, json).await,
        Cmd::Close { id, force } => close(id, ws, force).await,
        Cmd::Update { id, status, title, body, append, assignee, tags, deps, priority, kind, project, due, module, force } =>
            update(id, ws, status, title, body, append, assignee, tags, deps, priority, kind, project, due, module, force).await,
        Cmd::Start { id } => start(id, ws).await,
        Cmd::Deps { id, up, down, json } => deps(id, ws, up, down, json).await,
        Cmd::Rm { id, yes } => rm(id, ws, yes).await,
        Cmd::Modules { project, replace, stdin, json } => modules(ws, project, replace, stdin, json).await,
        Cmd::Meta { json } => meta(ws, json).await,
        Cmd::Whoami => whoami().await,
        Cmd::Mcp => serve_mcp().await,
        Cmd::Upgrade => {
            let cfg = config::load()?;
            upgrade::run(&cfg.url).await
        }
    }
}

async fn login() -> Result<()> {
    let mut cfg = config::load()?;
    let client = api::Client::new(&cfg.url);
    let start = client.device_start().await?;

    // Код печатается крупно и отдельно: его читают с экрана и набирают в
    // браузере, поэтому он не должен теряться среди прочего вывода.
    println!();
    println!("  Откройте:  {}", start.verification_url);
    println!("  Код:       {}", start.code);
    println!();
    println!("  Войдите рабочим аккаунтом. Жду…");

    let key = client.device_wait(&start.code, &start.device_secret, start.expires_in).await?;
    cfg.key = Some(key);
    config::save(&cfg)?;
    println!("  Готово. Ключ сохранён в {}", config::path()?.display());
    Ok(())
}

async fn ls(
    workspace: Option<String>,
    f: api::Filters,
    count: bool,
    limit: i64,
    offset: i64,
    json: bool,
) -> Result<()> {
    let started = std::time::Instant::now();
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;

    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("не указан воркспейс: задайте -W или workspace в .ntkrc")?;

    let client = api::Client::new(&cfg.url);

    if count {
        let n = client.count(key, &ws, &f).await?;
        if json {
            println!("{}", serde_json::json!({ "count": n }));
        } else {
            println!("{n}");
        }
        return Ok(());
    }

    let tickets = client.tickets(key, &ws, &f, limit, offset).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&tickets)?);
        return Ok(());
    }
    if tickets.is_empty() {
        println!("пусто{}", if f.all || f.assignee.is_some() { "" } else { " — попробуйте --all" });
        return Ok(());
    }
    for t in &tickets {
        // Ширины подобраны под id вида proj-xxxxxxxxxx и наши статусы.
        println!(
            "{:<16} {:<12} {:<10} {}",
            t.id,
            t.status,
            t.assignee.as_deref().unwrap_or("—"),
            t.title
        );
    }
    // Показываем, что список ОБРЕЗАН, и чем листать. Молча отдать 20 из
    // двух тысяч — значит соврать о размере очереди.
    print!("\nпоказано {}", tickets.len());
    if !f.all && f.assignee.is_none() {
        print!(", только свои (--all — все)");
    }
    if tickets.len() as i64 == limit {
        print!("; дальше: -o {}", offset + limit);
    }
    println!(" · took {} ms", started.elapsed().as_millis());
    Ok(())
}

/// Печать тикета целиком. Одна на show и walk: два разных вывода одного и того
/// же тикета — это два места, где однажды пропадёт поле, и заметят это не сразу.
fn print_ticket(t: &ntk_core::Ticket) {
    println!("# {}", t.title);
    println!("ID: {}", t.id);
    println!();
    println!("Status:   {}", t.status);
    if let Some(p) = &t.priority { println!("Priority: {p}"); }
    if let Some(k) = &t.kind { println!("Type:     {k}"); }
    if let Some(a) = &t.assignee { println!("Assignee: {a}"); }
    if let Some(p) = &t.project { println!("Project:  {p}"); }
    if let Some(m) = &t.module { println!("Module:   {m}"); }
    if !t.tags.is_empty() { println!("Tags:     {}", t.tags.join(", ")); }
    // Зависимости показываются всегда, когда они есть: именно их отсутствие в
    // выводе однажды заставило читать нарисованное дерево вместо данных.
    if !t.deps.is_empty() { println!("Deps:     {}", t.deps.join(", ")); }
    if let Some(d) = &t.due { println!("Due:      {d}"); }
    println!("Created:  {}", &t.created_at[..10.min(t.created_at.len())]);
    if let Some(s) = &t.started_at { println!("Started:  {}", &s[..10.min(s.len())]); }
    if let Some(c) = &t.closed_at { println!("Closed:   {}", &c[..10.min(c.len())]); }

    match t.body.as_deref() {
        Some(b) if !b.is_empty() => { println!(); println!("{b}"); }
        _ => {}
    }
}

/// Идентификатор сеанса обхода.
///
/// Придумывает КЛИЕНТ и хранит у себя — на сервере лежит только сам обход.
/// Два агента под одним ключом заводят разные идентификаторы и ходят
/// независимо; общий на человека означал бы общую память и один и тот же тикет
/// обоим.
fn walk_id(ws: &str, what: &str) -> Result<String> {
    use std::io::Write;
    let home = std::env::var("HOME").context("HOME не задан")?;
    let p = std::path::PathBuf::from(home).join(".config/ntk/walks.json");
    let key = format!("{ws}|{what}");
    let mut all: serde_json::Map<String, serde_json::Value> = std::fs::read_to_string(&p)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    if let Some(v) = all.get(&key).and_then(|v| v.as_str()) {
        return Ok(v.to_string());
    }
    let id: String = format!(
        "w{:x}{:x}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        std::process::id()
    );
    all.insert(key, serde_json::Value::String(id.clone()));
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut f = std::fs::File::create(&p)?;
    f.write_all(serde_json::to_string_pretty(&all)?.as_bytes())?;
    Ok(id)
}

fn describe(f: &api::Filters) -> String {
    let mut p = Vec::new();
    if let Some(v) = &f.status { p.push(format!("статус {v}")); }
    if let Some(v) = &f.assignee { p.push(format!("исполнитель {v}")); }
    if let Some(v) = &f.project { p.push(format!("проект {v}")); }
    if let Some(v) = &f.tag { p.push(format!("тег {v}{}", if f.strict { " целиком" } else { "" })); }
    if let Some(v) = &f.title { p.push(format!("заголовок «{v}»")); }
    if f.all { p.push("все, не только свои".into()); }
    if p.is_empty() { "без отбора".into() } else { p.join(", ") }
}

async fn walk(workspace: Option<String>, f: api::Filters, reset: bool, json: bool) -> Result<()> {
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("не указан воркспейс: задайте -W или workspace в .ntkrc")?;
    let id = walk_id(&ws, &describe(&f))?;

    let v = api::Client::new(&cfg.url).walk(key, &ws, &id, &f, reset).await?;
    if json {
        println!("{v}");
        return Ok(());
    }
    if v.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
        println!(
            "обход пройден: показано {} из {}",
            v.get("seen").and_then(|x| x.as_i64()).unwrap_or(0),
            v.get("total").and_then(|x| x.as_i64()).unwrap_or(0)
        );
        println!("начать заново: ntk walk --reset");
        return Ok(());
    }
    let t: ntk_core::Ticket = serde_json::from_value(v["ticket"].clone())?;
    // Счётчик в поток ошибок: тело тикета остаётся пригодным для конвейера.
    eprintln!(
        "— {} из {} — {}",
        v.get("at").and_then(|x| x.as_i64()).unwrap_or(0),
        v.get("total").and_then(|x| x.as_i64()).unwrap_or(0),
        v.get("what").and_then(|x| x.as_str()).unwrap_or("")
    );
    print_ticket(&t);
    Ok(())
}

async fn show(id: String, workspace: Option<String>, json: bool) -> Result<()> {
    let started = std::time::Instant::now();
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("не указан воркспейс: задайте -W или workspace в .ntkrc")?;

    let t = api::Client::new(&cfg.url).ticket(key, &ws, &id).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&t)?);
        return Ok(());
    }

    print_ticket(&t);
    eprintln!("· took {} ms", started.elapsed().as_millis());
    Ok(())
}

async fn next(workspace: Option<String>, prefer: Option<String>, json: bool) -> Result<()> {
    let started = std::time::Instant::now();
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("не указан воркспейс: задайте -W или workspace в .ntkrc")?;

    let taken = api::Client::new(&cfg.url).next(key, &ws, prefer.as_deref()).await?;

    match taken {
        None => {
            // Пусто — это ответ, а не ошибка: свободных тикетов может просто
            // не быть, и агент должен отличать это от сбоя.
            if json { println!("null"); } else { println!("свободных тикетов нет"); }
            Ok(())
        }
        Some(t) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&t)?);
            } else {
                println!("{}  {}", t.id, t.title);
                println!("статус: {}", t.status);
            }
            eprintln!("· took {} ms", started.elapsed().as_millis());
    Ok(())
        }
    }
}

/// Отдаёт инструменты по stdio. Ничего не печатает в stdout, кроме протокола:
/// любая посторонняя строка там ломает разбор на стороне клиента.
async fn serve_mcp() -> Result<()> {
    use rmcp::{transport::stdio, ServiceExt};
    let service = mcp::Ntk::new().serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}


#[allow(clippy::too_many_arguments)]
async fn create(
    title: String,
    workspace: Option<String>,
    project: Option<String>,
    priority: Option<String>,
    assignee: Option<String>,
    kind: Option<String>,
    status: Option<String>,
    tags: Option<String>,
    body: Option<String>,
    deps: Option<String>,
    module: Option<String>,
    json: bool,
) -> Result<()> {
    // Проект из .ntkrc, если не задан флагом: он там уже записан, и требовать
    // -P в каждом вызове значит просить человека повторить известное.
    let project = project.or_else(config::project_from_rc);

    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("не указан воркспейс: задайте -W или workspace в .ntkrc")?;

    let split = |s: Option<String>| -> Vec<String> {
        s.map(|v| v.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect())
            .unwrap_or_default()
    };
    let client = api::Client::new(&cfg.url);

    // Идентификатор назначает сервер. Раньше его придумывал клиент, и знание о
    // форме идентификатора жило в каждом клиенте отдельно: MCP-инструмент это
    // поле просто не слал, и заведение тикета через MCP не работало вовсе.
    let payload = serde_json::json!({
        // Воркспейс идёт в теле: ручка создания читает его оттуда, и
        // расхождение стоило мне одного прогона стенда.
        "workspace": ws,
        "title": title,
        "status": status,
        "priority": priority,
        "kind": kind,
        "assignee": assignee,
        "project": project,
        "tags": split(tags.clone()),
        "body": body,
        "deps": split(deps.clone()),
        "module": module,
    });
    let id = client
        .create(key, &ws, &payload)
        .await?
        .context("сервер не смог подобрать свободный идентификатор")?;

    if json {
        println!("{}", serde_json::json!({ "id": id }));
    } else {
        println!("{id}  {title}");
    }
    Ok(())
}

async fn close(id: String, workspace: Option<String>, force: bool) -> Result<()> {
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("не указан воркспейс: задайте -W или workspace в .ntkrc")?;

    // Передаётся только статус. Время закрытия ставит триггер при переходе:
    // подставь его здесь — и запишется момент выполнения команды вместо
    // момента, когда работа закончилась.
    let body = serde_json::json!({ "workspace": ws, "status": "done", "force": force });
    api::Client::new(&cfg.url).patch(key, &ws, &id, &body).await?;
    println!("{id} закрыт");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
async fn update(
    id: String,
    workspace: Option<String>,
    status: Option<String>,
    title: Option<String>,
    body: Option<String>,
    append: Option<String>,
    assignee: Option<String>,
    tags: Option<String>,
    deps: Option<String>,
    priority: Option<String>,
    kind: Option<String>,
    project: Option<String>,
    due: Option<String>,
    module: Option<String>,
    force: bool,
) -> Result<()> {
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("не указан воркспейс: задайте -W или workspace в .ntkrc")?;

    if status.is_none() && title.is_none() && body.is_none() && append.is_none()
        && assignee.is_none() && tags.is_none() && deps.is_none()
        && priority.is_none() && kind.is_none() && project.is_none() && due.is_none()
        && module.is_none()
    {
        // Команда без единого изменения молча ничего не делала бы и выглядела
        // успешной — это и есть тот отказ, который надо произнести вслух.
        anyhow::bail!("нечего менять: передайте хотя бы одно из -s, -p, -T, -P, --title, -b, -A, -a, -t, --dep, --due");
    }
    // Заменить и дописать разом — почти наверняка описка, а цена описки здесь
    // чужой разбор в теле тикета.
    if body.is_some() && append.is_some() {
        anyhow::bail!("-b и -A вместе не принимаются: либо заменить тело, либо дописать");
    }
    if append.as_deref().is_some_and(|a| a.trim().is_empty()) {
        anyhow::bail!("-A пуст: дописывать нечего");
    }

    // Каждый тег со знаком. Без знака отвергаем ЗДЕСЬ, до похода на сервер:
    // ошибку набора надо назвать сразу, а не через сеть.
    let tag_edits: Option<Vec<String>> = match tags.as_deref() {
        None => None,
        Some(t) => {
            let list: Vec<String> = t.split(',').map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect();
            for e in &list {
                if !e.starts_with('+') && !e.starts_with('-') {
                    anyhow::bail!("тег «{e}» без знака: нужен + или -");
                }
            }
            Some(list)
        }
    };

    // Три формы, как в старом --deps: "a,b" заменяет набор, "+a,-b" правит,
    // "" очищает. Здесь голый список НЕ ловушка, а описанное поведение, к
    // которому люди привыкли — в отличие от тегов, где документировались одни
    // дельты и голый список молча стирал остальные.
    let (dep_set, dep_edits) = match deps.as_deref() {
        None => (None, None),
        Some(d) if d.trim().is_empty() => (Some(Vec::new()), None),
        Some(d) => {
            let list: Vec<String> =
                d.split(',').map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect();
            let signed = list.iter().filter(|e| e.starts_with('+') || e.starts_with('-')).count();
            if signed == 0 {
                (Some(list), None)
            } else if signed == list.len() {
                (None, Some(list))
            } else {
                // Смешение старый инструмент принимал, считая голый элемент за
                // «+». Это ровно та двусмысленность, из-за которой мы потеряли
                // шесть тегов: одно и то же написание значит разное в
                // зависимости от соседей.
                anyhow::bail!(
                    "нельзя смешивать замену и правку: либо \"a,b\" целиком, либо каждый со знаком"
                );
            }
        }
    };

    let payload = serde_json::json!({
        "workspace": ws, "force": force,
        "status": status, "title": title, "body": body, "assignee": assignee,
        "body_append": append,
        "tag_edits": tag_edits,
        "dep_edits": dep_edits,
        "dep_set": dep_set,
        "priority": priority, "type": kind, "project": project, "due": due,
        "module": module,
    });
    let client = api::Client::new(&cfg.url);
    client.patch(key, &ws, &id, &payload).await?;

    // Успешный ответ — ещё не изменение. Сервер старее клиента молча
    // выбрасывает поля, о которых не знает: правка зависимостей однажды
    // ушла в никуда, а команда сказала «изменён». Поэтому эффект
    // подтверждается чтением, а не словом сервера.
    if dep_set.is_some() || dep_edits.is_some() {
        let tree = client.deps(key, &ws, &id).await?;
        let now: Vec<String> = tree
            .get("up")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.get("id").and_then(|i| i.as_str()).map(String::from)).collect())
            .unwrap_or_default();
        let expected_added: Vec<String> = dep_set
            .clone()
            .unwrap_or_default()
            .into_iter()
            .chain(dep_edits.clone().unwrap_or_default().into_iter()
                .filter(|e| e.starts_with('+'))
                .map(|e| e[1..].to_string()))
            .collect();
        let missing: Vec<&String> = expected_added
            .iter()
            .filter(|want| !now.iter().any(|have| have.eq_ignore_ascii_case(want)))
            .collect();
        if !missing.is_empty() {
            anyhow::bail!(
                "сервер ответил успехом, но зависимости не появились: {}\n\
                 скорее всего сервис старее клиента и выбросил поле, о котором не знает",
                missing.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
            );
        }
    }

    println!("{id} изменён");
    Ok(())
}

/// Взять КОНКРЕТНЫЙ тикет, в отличие от `next`, который берёт любой свободный.
///
/// Сервер отвечает 409, если тикет уже взят: захват проверяет статус в той же
/// транзакции, что и запись, поэтому «взял» здесь означает «взял», а не
/// «прочитал open секунду назад».
async fn start(id: String, workspace: Option<String>) -> Result<()> {
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("не указан воркспейс: задайте -W или workspace в .ntkrc")?;

    let taken = api::Client::new(&cfg.url).start(key, &ws, &id).await?;
    println!("{}  {}", taken.id, taken.title);
    println!("статус: {}", taken.status);
    Ok(())
}

async fn deps(id: String, workspace: Option<String>, up_only: bool, down_only: bool, json: bool) -> Result<()> {
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("не указан воркспейс: задайте -W или workspace в .ntkrc")?;

    let tree = api::Client::new(&cfg.url).deps(key, &ws, &id).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&tree)?);
        return Ok(());
    }

    let line = |v: &serde_json::Value| {
        let removed = v.get("removed").and_then(|x| x.as_bool()).unwrap_or(false);
        format!(
            "{:<16} {:<12} {}{}",
            v.get("id").and_then(|x| x.as_str()).unwrap_or(""),
            v.get("status").and_then(|x| x.as_str()).unwrap_or(""),
            v.get("title").and_then(|x| x.as_str()).unwrap_or(""),
            if removed { "  (убран)" } else { "" }
        )
    };
    let side = |name: &str| -> Vec<serde_json::Value> {
        tree.get(name).and_then(|v| v.as_array()).cloned().unwrap_or_default()
    };

    // Порядок сверху вниз: сначала то, чего тикет ждёт, потом он сам, потом
    // то, что ждёт его. Так читается, кто кого держит.
    // Без флагов показываются обе стороны — как в старом инструменте.
    let show_up = up_only || !down_only;
    let show_down = down_only || !up_only;
    let up = if show_up { side("up") } else { Vec::new() };
    if !up.is_empty() {
        println!("ждёт:");
        for v in &up { println!("  {}", line(v)); }
    }
    println!("{}", line(&tree));
    let down = if show_down { side("down") } else { Vec::new() };
    if !down.is_empty() {
        println!("его ждут:");
        for v in &down { println!("  {}", line(v)); }
    }
    if up.is_empty() && down.is_empty() {
        println!("(зависимостей нет ни в одну сторону)");
    }
    Ok(())
}

async fn rm(id: String, workspace: Option<String>, yes: bool) -> Result<()> {
    if !yes {
        // Спрашиваем, только когда есть кого спросить: в конвейере вопрос
        // повис бы навсегда, а агенты работают именно так.
        use std::io::{IsTerminal, Write};
        if std::io::stdin().is_terminal() {
            print!("убрать {id}? [y/N] ");
            std::io::stdout().flush().ok();
            let mut a = String::new();
            std::io::stdin().read_line(&mut a).ok();
            if !matches!(a.trim(), "y" | "yes" | "д" | "да") {
                println!("отменено");
                return Ok(());
            }
        }
    }
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("не указан воркспейс: задайте -W или workspace в .ntkrc")?;

    let waiting = api::Client::new(&cfg.url).remove(key, &ws, &id).await?;
    println!("{id} убран");
    // Тикеты, которые его ждали, названы вслух: их зависимость сохранилась, но
    // ждут они теперь то, чего не видно в списках, и знать об этом надо сразу.
    if !waiting.is_empty() {
        println!("на нём стояли: {}", waiting.join(", "));
    }
    Ok(())
}

async fn meta(workspace: Option<String>, json: bool) -> Result<()> {
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("не указан воркспейс: задайте -W или workspace в .ntkrc")?;

    let m = api::Client::new(&cfg.url).meta(key, &ws).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&m)?);
        return Ok(());
    }

    let list = |name: &str| -> Vec<String> {
        m.get(name)
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default()
    };

    println!("воркспейс: {ws}");
    let all = list("workspaces");
    if all.len() > 1 {
        println!("доступны:  {}", all.join(", "));
    }
    println!();
    println!("статусы:");
    if let Some(items) = m.get("statuses").and_then(|v| v.as_array()) {
        for s in items {
            let force = s.get("requires_force").and_then(|f| f.as_bool()).unwrap_or(false);
            println!(
                "  {:<12} {:<12} {}",
                s.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                s.get("group").and_then(|x| x.as_str()).unwrap_or(""),
                // Гард показывается словами: «нужен --force» понятнее, чем
                // requires_force=true, а знать это надо до правки, не после.
                if force { "правка требует --force" } else { "" }
            );
        }
    }
    println!();
    println!("приоритеты: {}", list("priorities").join(", "));
    println!("проекты:    {}", list("projects").join(", "));
    // Модули печатаются с проектом: одно и то же имя в двух проектах — разные
    // модули, и список без проекта вводил бы в заблуждение ровно там, где это
    // важнее всего.
    if let Some(mods) = m.get("modules").and_then(|v| v.as_array()) {
        if !mods.is_empty() {
            let names: Vec<String> = mods
                .iter()
                .map(|x| format!(
                    "{}/{}",
                    x.get("project").and_then(|p| p.as_str()).unwrap_or(""),
                    x.get("name").and_then(|n| n.as_str()).unwrap_or("")
                ))
                .collect();
            println!("модули:     {}", names.join(", "));
        }
    }
    if let Some(people) = m.get("people").and_then(|v| v.as_array()) {
        let names: Vec<String> = people
            .iter()
            .map(|p| {
                let id = p.get("id").and_then(|x| x.as_str()).unwrap_or("");
                match p.get("kind").and_then(|x| x.as_str()) {
                    Some("agent") => format!("{id} (агент)"),
                    _ => id.to_string(),
                }
            })
            .collect();
        println!("люди:       {}", names.join(", "));
    }
    Ok(())
}

/// Кто я и куда мне можно. Отвечает на первый вопрос после входа: что
/// подставлять в -W.
async fn whoami() -> Result<()> {
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let (user, ws) = api::Client::new(&cfg.url).me(key).await?;
    println!("вы: {user}");
    if ws.is_empty() {
        println!("воркспейсов нет — обратитесь к администратору");
    } else {
        println!("воркспейсы: {}", ws.join(", "));
    }
    Ok(())
}

async fn modules(
    workspace: Option<String>,
    project: Option<String>,
    replace: bool,
    stdin: bool,
    json: bool,
) -> Result<()> {
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("не указан воркспейс: задайте -W или workspace в .ntkrc")?;
    let client = api::Client::new(&cfg.url);

    if replace {
        let project = project.context("замена списка требует проекта: -P")?;
        if !stdin {
            // Замена набора необратима для тех, кто исчезнет, поэтому источник
            // называется явно. Молчаливое чтение stdin означало бы, что
            // забытый флаг стирает реестр.
            anyhow::bail!("укажите --stdin: список читается со стандартного ввода, по имени в строке");
        }
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
            .context("не удалось прочитать список со стандартного ввода")?;
        let list: Vec<String> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(String::from)
            .collect();
        if list.is_empty() {
            anyhow::bail!("список пуст: замена стёрла бы весь реестр проекта; если это намерение, передайте хотя бы один модуль");
        }
        let r = client.replace_modules(key, &ws, &project, &list).await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&r)?);
            return Ok(());
        }
        let names = |k: &str| -> Vec<String> {
            r.get(k).and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default()
        };
        println!("{project}: всего {}", r.get("total").and_then(|t| t.as_i64()).unwrap_or(0));
        for (label, key) in [("добавлено", "added"), ("в архив", "archived"),
                             ("удалено", "deleted"), ("возвращено", "restored")] {
            let v = names(key);
            if !v.is_empty() { println!("  {label}: {}", v.join(", ")); }
        }
        return Ok(());
    }

    let m = client.meta(key, &ws).await?;
    let all = m.get("modules").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    let rows: Vec<&serde_json::Value> = all
        .iter()
        .filter(|x| match project.as_deref() {
            None => true,
            Some(p) => x.get("project").and_then(|v| v.as_str()) == Some(p),
        })
        .collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("модулей нет");
        return Ok(());
    }
    for x in rows {
        // Архивный помечается словом: список без пометки читался бы как
        // перечень доступных, а половина из них выбору не подлежит.
        let archived = x.get("archived").and_then(|v| v.as_bool()).unwrap_or(false);
        println!(
            "{:<12} {:<20} {}",
            x.get("project").and_then(|v| v.as_str()).unwrap_or(""),
            x.get("name").and_then(|v| v.as_str()).unwrap_or(""),
            if archived { "в архиве" } else { "" }
        );
    }
    Ok(())
}
