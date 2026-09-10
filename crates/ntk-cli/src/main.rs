//! ntk — клиент. Одна команда логина и чтение тикетов.

mod api;
mod config;
mod mcp;
mod upgrade;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ntk", about = "Tickets: one queue for agents and people", version)]
struct Cli {
    // Accepted both before and after the command: in the old tool the flag was
    // global, and people type it up front out of habit.
    #[arg(short = 'W', long, global = true, help = ntk_core::tools::CLI_WORKSPACE.desc)]
    workspace: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Sign in through the browser. The key is never typed by hand and never mailed.
    Login,
    #[command(about = ntk_core::tools::about("ntk_ls"), long_about = ntk_core::tools::desc("ntk_ls"))]
    Ls {
        #[arg(short = 's', long, help = ntk_core::tools::arg("ntk_ls", "status"))]
        status: Option<String>,
        #[arg(short = 't', long, help = ntk_core::tools::arg("ntk_ls", "tag"))]
        tag: Option<String>,
        #[arg(long, help = ntk_core::tools::arg("ntk_ls", "strict"))]
        strict: bool,
        #[arg(short = 'a', long, help = ntk_core::tools::arg("ntk_ls", "assignee"))]
        assignee: Option<String>,
        #[arg(short = 'P', long, help = ntk_core::tools::arg("ntk_ls", "project"))]
        project: Option<String>,
        #[arg(long, help = ntk_core::tools::arg("ntk_ls", "module"))]
        module: Option<String>,
        #[arg(short = 'q', long, help = ntk_core::tools::arg("ntk_ls", "title"))]
        title: Option<String>,
        #[arg(long, help = ntk_core::tools::arg("ntk_ls", "stale"))]
        stale: Option<i64>,
        #[arg(long, help = ntk_core::tools::arg("ntk_ls", "count"))]
        count: bool,
        #[arg(short = 'n', long, default_value_t = 50, help = ntk_core::tools::arg("ntk_ls", "limit"))]
        limit: i64,
        #[arg(short = 'o', long, default_value_t = 0, help = ntk_core::tools::arg("ntk_ls", "offset"))]
        offset: i64,
        #[arg(long, help = ntk_core::tools::arg("ntk_ls", "all"))]
        all: bool,
        #[arg(long, help = "Print JSON instead of a table.")]
        json: bool,
    },
    #[command(about = ntk_core::tools::about("ntk_walk"), long_about = ntk_core::tools::desc("ntk_walk"))]
    Walk {
        #[arg(long, help = ntk_core::tools::arg("ntk_walk", "module"))]
        module: Option<String>,
        #[arg(short = 's', long, help = ntk_core::tools::arg("ntk_walk", "status"))]
        status: Option<String>,
        #[arg(short = 't', long, help = ntk_core::tools::arg("ntk_walk", "tag"))]
        tag: Option<String>,
        #[arg(long, help = ntk_core::tools::arg("ntk_walk", "strict"))]
        strict: bool,
        #[arg(short = 'q', long, help = ntk_core::tools::arg("ntk_walk", "title"))]
        title: Option<String>,
        #[arg(short = 'a', long, help = ntk_core::tools::arg("ntk_walk", "assignee"))]
        assignee: Option<String>,
        #[arg(short = 'P', long, help = ntk_core::tools::arg("ntk_walk", "project"))]
        project: Option<String>,
        #[arg(long, help = ntk_core::tools::arg("ntk_walk", "all"))]
        all: bool,
        #[arg(long, help = ntk_core::tools::arg("ntk_walk", "reset"))]
        reset: bool,
        #[arg(long, help = "Print JSON instead of a table.")]
        json: bool,
    },
    #[command(about = ntk_core::tools::about("ntk_show"), long_about = ntk_core::tools::desc("ntk_show"))]
    Show {
        #[arg(help = ntk_core::tools::arg("ntk_show", "id"))]
        id: String,
        #[arg(long, help = "Print JSON instead of a table.")]
        json: bool,
    },
    #[command(about = ntk_core::tools::about("ntk_whoami"), long_about = ntk_core::tools::desc("ntk_whoami"))]
    Whoami,
    /// Upgrade to the latest version.
    ///
    /// What is downloaded is checked by checksum AND by signature before it
    /// replaces the current binary.
    Upgrade,
    /// Serve the same commands over MCP — for Claude Desktop and agents.
    ///
    /// Speaks over stdio: the client starts this binary and talks to it through
    /// standard input and output. No port, no installation.
    Mcp,
    #[command(about = ntk_core::tools::about("ntk_create"), long_about = ntk_core::tools::desc("ntk_create"))]
    Create {
        #[arg(help = ntk_core::tools::arg("ntk_create", "title"))]
        title: String,
        #[arg(short = 'P', long, help = ntk_core::tools::arg("ntk_create", "project"))]
        project: Option<String>,
        #[arg(short = 'p', long, help = ntk_core::tools::arg("ntk_create", "priority"))]
        priority: Option<String>,
        #[arg(short = 'a', long, help = ntk_core::tools::arg("ntk_create", "assignee"))]
        assignee: Option<String>,
        #[arg(short = 'T', long = "type", help = ntk_core::tools::arg("ntk_create", "type"))]
        kind: Option<String>,
        #[arg(short = 's', long, help = ntk_core::tools::arg("ntk_create", "status"))]
        status: Option<String>,
        #[arg(short = 't', long, help = "Tags, comma-separated. Creating sets the whole set at once.")]
        tags: Option<String>,
        #[arg(short = 'b', long, short_alias = 'd', help = ntk_core::tools::arg("ntk_create", "body"))]
        body: Option<String>,
        #[arg(long, help = "Identifiers of the tickets this one waits for, comma-separated.")]
        deps: Option<String>,
        #[arg(long, help = ntk_core::tools::arg("ntk_create", "module"))]
        module: Option<String>,
        #[arg(long, help = ntk_core::tools::arg("ntk_create", "skip_search"))]
        skip_search: bool,
        #[arg(long, help = "Print JSON instead of a table.")]
        json: bool,
    },
    #[command(about = ntk_core::tools::about("ntk_update"), long_about = ntk_core::tools::desc("ntk_update"))]
    Update {
        #[arg(help = ntk_core::tools::arg("ntk_update", "id"))]
        id: String,
        #[arg(short = 's', long, help = ntk_core::tools::arg("ntk_update", "status"))]
        status: Option<String>,
        #[arg(long, help = ntk_core::tools::arg("ntk_update", "title"))]
        title: Option<String>,
        #[arg(short = 'b', long, short_alias = 'd', help = ntk_core::tools::arg("ntk_update", "body"))]
        body: Option<String>,
        #[arg(short = 'A', long, help = ntk_core::tools::arg("ntk_update", "body_append"))]
        append: Option<String>,
        #[arg(short = 'a', long, help = ntk_core::tools::arg("ntk_update", "assignee"))]
        assignee: Option<String>,
        #[arg(short = 't', long = "tags", visible_alias = "tag", allow_hyphen_values = true, help = "Tag edits, comma-separated, each with a sign: \"+alpha,-legacy\". The sign is required, otherwise \"add\" will one day turn out to be \"replace everything\".")]
        tags: Option<String>,
        #[arg(long = "deps", visible_alias = "dep", allow_hyphen_values = true, help = "Dependencies, three forms: \"a,b\" REPLACES the whole set, \"+a,-b\" adds and removes, \"\" clears every one. The forms must not be mixed: a bare item next to a signed one once meant \"add\", and links were lost on that ambiguity.")]
        deps: Option<String>,
        #[arg(short = 'p', long, help = ntk_core::tools::arg("ntk_update", "priority"))]
        priority: Option<String>,
        #[arg(short = 'T', long = "type", help = ntk_core::tools::arg("ntk_update", "type"))]
        kind: Option<String>,
        #[arg(short = 'P', long, help = ntk_core::tools::arg("ntk_update", "project"))]
        project: Option<String>,
        #[arg(long, help = ntk_core::tools::arg("ntk_update", "due"))]
        due: Option<String>,
        #[arg(long, help = ntk_core::tools::arg("ntk_update", "module"))]
        module: Option<String>,
        #[arg(long, help = ntk_core::tools::arg("ntk_update", "force"))]
        force: bool,
    },
    #[command(about = ntk_core::tools::about("ntk_start"), long_about = ntk_core::tools::desc("ntk_start"))]
    Start {
        #[arg(help = ntk_core::tools::arg("ntk_start", "id"))]
        id: String,
    },
    #[command(about = ntk_core::tools::about("ntk_deps"), long_about = ntk_core::tools::desc("ntk_deps"))]
    Deps {
        #[arg(help = ntk_core::tools::arg("ntk_deps", "id"))]
        id: String,
        #[arg(long, conflicts_with = "down", help = "Only what this ticket stands on.")]
        up: bool,
        #[arg(long, help = "Only what stands on this ticket.")]
        down: bool,
        #[arg(long, help = "Print JSON instead of a table.")]
        json: bool,
    },
    #[command(about = ntk_core::tools::about("ntk_rm"), long_about = ntk_core::tools::desc("ntk_rm"))]
    Rm {
        #[arg(help = ntk_core::tools::arg("ntk_rm", "id"))]
        id: String,
        #[arg(short = 'y', long, help = "Do not ask for confirmation.")]
        yes: bool,
    },
    #[command(about = ntk_core::tools::about("ntk_modules"), long_about = ntk_core::tools::modules_help())]
    Modules {
        #[arg(short = 'P', long, help = ntk_core::tools::arg("ntk_modules", "project"))]
        project: Option<String>,
        #[arg(long, help = "Replace the project's module list with what arrives on standard input, one name per line. The list is taken as COMPLETE.")]
        replace: bool,
        #[arg(long, conflicts_with = "replace", help = "Add the named modules WITHOUT touching the rest of the registry. Nothing leaves the live set — unlike --replace, which wants the full list and removes whatever is missing from it.")]
        add: bool,
        #[arg(long, help = "Read the list from standard input. Spelled out on purpose, so that replacing a set never happens by oversight.")]
        stdin: bool,
        #[arg(long, help = "Print JSON instead of a table.")]
        json: bool,
    },
    /// Project lifecycle: retire an emptied name and move tickets wholesale.
    ///
    /// There is deliberately no rename: a project id sits as the prefix of every
    /// ticket identifier, and those are primary keys. "Rename" here means moving
    /// the tickets and retiring the emptied name — two steps.
    Projects {
        /// Retire an emptied project from the choices. A project that still has tickets is not retired.
        #[arg(long)]
        archive: Option<String>,
        /// Bring a project back into the choices.
        #[arg(long)]
        unarchive: Option<String>,
        /// Move ALL of this project's tickets. Requires --to.
        #[arg(long = "move")]
        move_from: Option<String>,
        /// Where to move them.
        #[arg(long)]
        to: Option<String>,
        #[arg(long, help = "Print JSON instead of a table.")]
        json: bool,
    },
    #[command(about = ntk_core::tools::about("ntk_find"), long_about = ntk_core::tools::desc("ntk_find"))]
    Find {
        #[arg(help = ntk_core::tools::arg("ntk_find", "text"))]
        text: Option<String>,
        #[arg(short = 'b', long, help = ntk_core::tools::arg("ntk_find", "body"))]
        body: Option<String>,
        #[arg(long, help = ntk_core::tools::arg("ntk_find", "id"))]
        id: Option<String>,
        #[arg(short = 'n', long, help = ntk_core::tools::arg("ntk_find", "limit"))]
        limit: Option<i64>,
        #[arg(long, help = ntk_core::tools::arg("ntk_find", "min_score"))]
        min_score: Option<f64>,
        #[arg(long, help = "Print JSON instead of a table.")]
        json: bool,
    },
    #[command(about = ntk_core::tools::about("ntk_meta"), long_about = ntk_core::tools::desc("ntk_meta"))]
    Meta {
        #[arg(long, help = "Print JSON instead of a table.")]
        json: bool,
    },
    #[command(about = ntk_core::tools::about("ntk_close"), long_about = ntk_core::tools::desc("ntk_close"))]
    Close {
        #[arg(help = ntk_core::tools::arg("ntk_close", "id"))]
        id: String,
        #[arg(long, help = ntk_core::tools::arg("ntk_close", "force"))]
        force: bool,
    },
    #[command(about = ntk_core::tools::about("ntk_next"), long_about = ntk_core::tools::desc("ntk_next"))]
    Next {
        #[arg(long, help = ntk_core::tools::arg("ntk_next", "prefer"))]
        prefer: Option<String>,
        #[arg(short = 't', long, help = ntk_core::tools::arg("ntk_next", "tag"))]
        tag: Option<String>,
        #[arg(long, help = ntk_core::tools::arg("ntk_next", "strict"))]
        strict: bool,
        #[arg(short = 'P', long, help = ntk_core::tools::arg("ntk_next", "project"))]
        project: Option<String>,
        #[arg(long, help = ntk_core::tools::arg("ntk_next", "module"))]
        module: Option<String>,
        #[arg(long, conflicts_with = "module", help = ntk_core::tools::arg("ntk_next", "has_module"))]
        has_module: bool,
        #[arg(short = 'a', long, help = ntk_core::tools::arg("ntk_next", "assignee"))]
        assignee: Option<String>,
        #[arg(long, help = ntk_core::tools::arg("ntk_next", "dry_run"))]
        dry_run: bool,
        #[arg(long, help = "Print JSON instead of a table.")]
        json: bool,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let ws = cli.workspace.clone();
    match cli.cmd {
        Cmd::Login => login().await,
        Cmd::Ls { status, tag, strict, title, assignee, project, module, stale, count, limit, offset, all, json } => {
            let f = api::Filters { status, tag, title, assignee, project, module, strict, all, stale };
            ls(ws, f, count, limit, offset, json).await
        }
        Cmd::Walk { status, tag, strict, title, assignee, project, module, all, reset, json } => {
            let f = api::Filters { status, tag, title, assignee, project, module, strict, all, stale: None };
            walk(ws, f, reset, json).await
        }
        Cmd::Projects { archive, unarchive, move_from, to, json } =>
            projects(ws, archive, unarchive, move_from, to, json).await,
        Cmd::Show { id, json } => show(id, ws, json).await,
        Cmd::Next { prefer, tag, strict, project, module, has_module, assignee, dry_run, json } =>
            next(ws, prefer, tag, strict, project, module, has_module, assignee, dry_run, json).await,
        Cmd::Create { title, project, priority, assignee, kind, status, tags, body, deps, module, skip_search, json } =>
            create(title, ws, project, priority, assignee, kind, status, tags, body, deps, module, skip_search, json).await,
        Cmd::Close { id, force } => close(id, ws, force).await,
        Cmd::Update { id, status, title, body, append, assignee, tags, deps, priority, kind, project, due, module, force } =>
            update(id, ws, status, title, body, append, assignee, tags, deps, priority, kind, project, due, module, force).await,
        Cmd::Start { id } => start(id, ws).await,
        Cmd::Deps { id, up, down, json } => deps(id, ws, up, down, json).await,
        Cmd::Rm { id, yes } => rm(id, ws, yes).await,
        Cmd::Modules { project, replace, add, stdin, json } =>
            modules(ws, project, replace, add, stdin, json).await,
        Cmd::Find { text, body, id, limit, min_score, json } =>
            find(ws, text, body, id, limit, min_score, json).await,
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
    println!("  Open:      {}", start.verification_url);
    println!("  Code:      {}", start.code);
    println!();
    println!("  Sign in with your work account. Waiting…");

    let key = client.device_wait(&start.code, &start.device_secret, start.expires_in).await?;
    cfg.key = Some(key);
    config::save(&cfg)?;
    println!("  Done. The key is saved in {}", config::path()?.display());
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
        .context("no workspace given: pass -W, or set workspace in .ntkrc")?;

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
        println!("empty{}", if f.all || f.assignee.is_some() { "" } else { " — try --all" });
        return Ok(());
    }
    for t in &tickets {
        // Ширины подобраны под id вида proj-xxxxxxxxxx и наши статусы.
        println!(
            "{:<16} {:<12} {:<6} {:<10} {}",
            t.id,
            t.status,
            days_in_status(t.current_status_at.as_deref()),
            t.assignee.as_deref().unwrap_or("—"),
            t.title
        );
    }
    // Показываем, что список ОБРЕЗАН, и чем листать. Молча отдать 20 из
    // двух тысяч — значит соврать о размере очереди.
    print!("\nshown {}", tickets.len());
    if !f.all && f.assignee.is_none() {
        print!(", yours only (--all for everyone's)");
    }
    if tickets.len() as i64 == limit {
        print!("; next: -o {}", offset + limit);
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
    let home = std::env::var("HOME").context("HOME is not set")?;
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
    if let Some(v) = &f.status { p.push(format!("status {v}")); }
    if let Some(v) = &f.assignee { p.push(format!("assignee {v}")); }
    if let Some(v) = &f.project { p.push(format!("project {v}")); }
    // Модуль обязан быть здесь, а не только в запросе: этой строкой различаются
    // СЕАНСЫ обхода (walk_id). Без него обходы с разным --module делят один
    // курсор, и второй продолжает с того места, где кончился первый.
    if let Some(v) = &f.module { p.push(format!("module {v}")); }
    if let Some(v) = &f.tag { p.push(format!("tag {v}{}", if f.strict { " exactly" } else { "" })); }
    if let Some(v) = &f.title { p.push(format!("title {v:?}")); }
    if f.all { p.push("everyone's, not just mine".into()); }
    if p.is_empty() { "no filter".into() } else { p.join(", ") }
}

async fn walk(workspace: Option<String>, f: api::Filters, reset: bool, json: bool) -> Result<()> {
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("no workspace given: pass -W, or set workspace in .ntkrc")?;
    let id = walk_id(&ws, &describe(&f))?;

    let v = api::Client::new(&cfg.url).walk(key, &ws, &id, &f, reset).await?;
    if json {
        println!("{v}");
        return Ok(());
    }
    if v.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
        println!(
            "walk finished: {} of {} shown",
            v.get("seen").and_then(|x| x.as_i64()).unwrap_or(0),
            v.get("total").and_then(|x| x.as_i64()).unwrap_or(0)
        );
        println!("start over: ntk walk --reset");
        return Ok(());
    }
    let t: ntk_core::Ticket = serde_json::from_value(v["ticket"].clone())?;
    // Счётчик в поток ошибок: тело тикета остаётся пригодным для конвейера.
    eprintln!(
        "— {} of {} — {}",
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
        .context("no workspace given: pass -W, or set workspace in .ntkrc")?;

    let t = api::Client::new(&cfg.url).ticket(key, &ws, &id).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&t)?);
        return Ok(());
    }

    print_ticket(&t);
    eprintln!("· took {} ms", started.elapsed().as_millis());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn next(
    workspace: Option<String>,
    prefer: Option<String>,
    tag: Option<String>,
    strict: bool,
    project: Option<String>,
    module: Option<String>,
    has_module: bool,
    assignee: Option<String>,
    dry_run: bool,
    json: bool,
) -> Result<()> {
    let started = std::time::Instant::now();
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("no workspace given: pass -W, or set workspace in .ntkrc")?;

    let pick = api::Pick {
        tag: tag.as_deref(),
        strict,
        project: project.as_deref(),
        module: module.as_deref(),
        has_module,
        assignee: assignee.as_deref(),
        dry_run,
    };
    let taken = api::Client::new(&cfg.url).next(key, &ws, prefer.as_deref(), &pick).await?;

    match taken {
        None => {
            // Пусто — это ответ, а не ошибка: свободных тикетов может просто
            // не быть, и агент должен отличать это от сбоя.
            if json { println!("null"); } else { println!("no free tickets"); }
            Ok(())
        }
        Some(t) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&t)?);
            } else {
                println!("{}  {}", t.id, t.title);
                println!("status: {}", t.status);
                // Сказать вслух обязательно: иначе вывод неотличим от захвата,
                // и человек уйдёт работать над тикетом, который ему не выдан.
                if dry_run { println!("NOT taken: this is what would have been taken"); }
            }
            eprintln!("· took {} ms", started.elapsed().as_millis());
    Ok(())
        }
    }
}

/// Serves the tools over stdio. Nothing but protocol goes to stdout: one stray
/// line there breaks parsing on the client side.
///
/// The stream is filtered before rmcp sees it, and that is not decoration.
///
/// Newer clients OPEN with a probe of their own rather than with `initialize`:
/// Gemini (antigravity-client) sends `server/discover` carrying protocol
/// version 2026-07-28, with the client info moved into `_meta`. rmcp knows only
/// the older handshake — it waits for `initialize`, sees something else and
/// CLOSES THE CONNECTION. The client then has nothing to fall back to, and the
/// server shows up as broken: "connection closed: initialized request".
///
/// The two servers that do work in that client answer such a probe with a plain
/// `-32601 method not found` and STAY ALIVE — after which the client falls back
/// to `initialize` and everything proceeds. That is the whole difference, and
/// that is what is reproduced here: before `initialize`, an unknown request is
/// answered with -32601 and never forwarded; an unknown notification is
/// dropped. After `initialize` the stream passes through untouched.
///
/// Written as a filter rather than as support for the new protocol on purpose:
/// answering a probe we do not implement would be a lie, while refusing it
/// honestly is what the specification is for.
async fn serve_mcp() -> Result<()> {
    use rmcp::ServiceExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    // The filter writes here, rmcp reads the other end.
    let (mut to_server, server_in) = tokio::io::duplex(64 * 1024);
    // rmcp writes here, the pump copies the other end to the real stdout.
    let (server_out, from_server) = tokio::io::duplex(64 * 1024);

    // One writer owns stdout. Two producers write to it — rmcp's answers and
    // our own refusals — and interleaved halves of two JSON lines would be
    // unparseable garbage.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        let mut out = tokio::io::stdout();
        while let Some(s) = rx.recv().await {
            if out.write_all(s.as_bytes()).await.is_err() {
                break;
            }
            let _ = out.flush().await;
        }
    });

    let pump = tx.clone();
    tokio::spawn(async move {
        let mut r = BufReader::new(from_server);
        let mut line = String::new();
        loop {
            line.clear();
            match r.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if pump.send(line.clone()).is_err() {
                break;
            }
        }
    });

    let refuse = tx;
    tokio::spawn(async move {
        let mut r = BufReader::new(tokio::io::stdin());
        let mut line = String::new();
        let mut handshaken = false;
        loop {
            line.clear();
            match r.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if line.trim().is_empty() {
                continue;
            }
            if !handshaken {
                let parsed: Option<serde_json::Value> = serde_json::from_str(&line).ok();
                let method = parsed
                    .as_ref()
                    .and_then(|v| v.get("method"))
                    .and_then(|m| m.as_str())
                    .unwrap_or_default();
                if method == "initialize" {
                    handshaken = true;
                } else {
                    // A request gets an honest refusal; a notification is
                    // dropped, because answering one is itself a protocol error.
                    if let Some(id) = parsed.as_ref().and_then(|v| v.get("id")).cloned() {
                        let err = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": id,
                            "error": {
                                "code": -32601,
                                "message": format!("unknown method {method}")
                            }
                        });
                        if refuse.send(format!("{err}\n")).is_err() {
                            break;
                        }
                    }
                    continue;
                }
            }
            if to_server.write_all(line.as_bytes()).await.is_err() {
                break;
            }
        }
        // stdin ended: closing our side lets rmcp finish rather than hang.
        drop(to_server);
    });

    let service = mcp::Ntk::new().serve((server_in, server_out)).await?;
    service.waiting().await?;
    Ok(())
}


#[allow(clippy::too_many_arguments)]
/// Проверяет длину ДО отправки, чтобы не гонять по сети то, что будет отвергнуто.
///
/// Порог НЕ ЗАШИТ здесь и не хранится копией: он спрашивается у сервиса
/// (`meta.limits`), потому что живёт в таблице и его двигают без выпуска. Своя
/// копия числа разошлась бы с настоящим порогом в первый же раз — сегодня мы
/// уже дважды чинили последствия ровно такого расхождения.
///
/// Спрашиваем не всегда, а только когда тело действительно велико: лишний вызов
/// стоит около 230 мс, и платить их на каждом коротком тикете незачем. Порог
/// «когда спросить» намеренно грубый — он про экономию сети, а не про политику.
const ASK_LIMITS_OVER: usize = 4096;

async fn refuse_if_too_long(
    c: &api::Client,
    key: &str,
    ws: &str,
    title: Option<&str>,
    body: Option<&str>,
) -> Result<()> {
    let biggest = body.map(|b| b.chars().count()).unwrap_or(0);
    if biggest <= ASK_LIMITS_OVER {
        return Ok(());
    }
    let m = c.meta(key, ws).await?;
    let limit = |f: &str| -> Option<usize> {
        m.get("limits")?.get(f)?.as_u64().map(|v| v as usize)
    };
    for (field, value, what) in [
        ("title", title, "заголовок"),
        ("body", body, "тело"),
    ] {
        let Some(v) = value else { continue };
        let got = v.chars().count();
        if let Some(max) = limit(field) {
            if got > max {
                anyhow::bail!(
                    "{what} длиннее предела: {got} символов при {max}. Разбейте работу на \
                     отдельные тикеты и свяжите через --deps. Отправка не выполнена."
                );
            }
        }
    }
    Ok(())
}

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
    skip_search: bool,
    json: bool,
) -> Result<()> {
    // Проект из .ntkrc, если не задан флагом: он там уже записан, и требовать
    // -P в каждом вызове значит просить человека повторить известное.
    let project = project.or_else(config::project_from_rc);

    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("no workspace given: pass -W, or set workspace in .ntkrc")?;

    let split = |s: Option<String>| -> Vec<String> {
        s.map(|v| v.split(',').map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect())
            .unwrap_or_default()
    };
    let client = api::Client::new(&cfg.url);
    refuse_if_too_long(&client, key, &ws, Some(&title), body.as_deref()).await?;

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
        "type": kind,
        "assignee": assignee,
        "project": project,
        "tags": split(tags.clone()),
        "body": body,
        "deps": split(deps.clone()),
        "module": module,
        "skip_search": skip_search,
    });
    let id = client
        .create(key, &ws, &payload)
        .await?
        .context("the server could not find a free identifier")?;

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
        .context("no workspace given: pass -W, or set workspace in .ntkrc")?;

    // Передаётся только статус. Время закрытия ставит триггер при переходе:
    // подставь его здесь — и запишется момент выполнения команды вместо
    // момента, когда работа закончилась.
    let body = serde_json::json!({ "workspace": ws, "status": "done", "force": force });
    api::Client::new(&cfg.url).patch(key, &ws, &id, &body).await?;
    println!("{id} closed");
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
        .context("no workspace given: pass -W, or set workspace in .ntkrc")?;

    if status.is_none() && title.is_none() && body.is_none() && append.is_none()
        && assignee.is_none() && tags.is_none() && deps.is_none()
        && priority.is_none() && kind.is_none() && project.is_none() && due.is_none()
        && module.is_none()
    {
        // Команда без единого изменения молча ничего не делала бы и выглядела
        // успешной — это и есть тот отказ, который надо произнести вслух.
        anyhow::bail!("nothing to change: pass at least one of -s, -p, -T, -P, --title, -b, -A, -a, -t, --dep, --due");
    }
    // Заменить и дописать разом — почти наверняка описка, а цена описки здесь
    // чужой разбор в теле тикета.
    if body.is_some() && append.is_some() {
        anyhow::bail!("-b and -A are not accepted together: either replace the body or append to it");
    }
    if append.as_deref().is_some_and(|a| a.trim().is_empty()) {
        anyhow::bail!("-A is empty: nothing to append");
    }

    // Каждый тег со знаком. Без знака отвергаем ЗДЕСЬ, до похода на сервер:
    // ошибку набора надо назвать сразу, а не через сеть.
    let tag_edits: Option<Vec<String>> = match tags.as_deref() {
        None => None,
        Some(t) => {
            let list: Vec<String> = t.split(',').map(str::trim).filter(|x| !x.is_empty()).map(String::from).collect();
            for e in &list {
                if !e.starts_with('+') && !e.starts_with('-') {
                    anyhow::bail!("tag {e:?} has no sign: use + or -");
                }
            }
            Some(list)
        }
    };

    // Три формы, как в старом --deps: "a,b" replaces the set, "+a,-b" правит,
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
                    "signs must not be mixed with a plain list: either \"a,b\" as a whole, or each with a sign"
                );
            }
        }
    };

    let client = api::Client::new(&cfg.url);
    // При дописывании длину итога знает только сервис — здесь проверяем то, что
    // отправляем, чтобы не гнать по сети заведомо отвергаемое.
    refuse_if_too_long(&client, key, &ws, title.as_deref(), body.as_deref().or(append.as_deref())).await?;

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

    println!("{id} changed");
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
        .context("no workspace given: pass -W, or set workspace in .ntkrc")?;

    let taken = api::Client::new(&cfg.url).start(key, &ws, &id).await?;
    println!("{}  {}", taken.id, taken.title);
    println!("status: {}", taken.status);
    Ok(())
}

async fn deps(id: String, workspace: Option<String>, up_only: bool, down_only: bool, json: bool) -> Result<()> {
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("no workspace given: pass -W, or set workspace in .ntkrc")?;

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
            if removed { "  (removed)" } else { "" }
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
        println!("waits for:");
        for v in &up { println!("  {}", line(v)); }
    }
    println!("{}", line(&tree));
    let down = if show_down { side("down") } else { Vec::new() };
    if !down.is_empty() {
        println!("waited on by:");
        for v in &down { println!("  {}", line(v)); }
    }
    if up.is_empty() && down.is_empty() {
        println!("(no dependencies in either direction)");
    }
    Ok(())
}

async fn rm(id: String, workspace: Option<String>, yes: bool) -> Result<()> {
    if !yes {
        // Спрашиваем, только когда есть кого спросить: в конвейере вопрос
        // повис бы навсегда, а агенты работают именно так.
        use std::io::{IsTerminal, Write};
        if std::io::stdin().is_terminal() {
            print!("remove {id}? [y/N] ");
            std::io::stdout().flush().ok();
            let mut a = String::new();
            std::io::stdin().read_line(&mut a).ok();
            if !matches!(a.trim(), "y" | "yes" | "д" | "да") {
                println!("cancelled");
                return Ok(());
            }
        }
    }
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("no workspace given: pass -W, or set workspace in .ntkrc")?;

    let waiting = api::Client::new(&cfg.url).remove(key, &ws, &id).await?;
    println!("{id} removed");
    // Тикеты, которые его ждали, названы вслух: их зависимость сохранилась, но
    // ждут они теперь то, чего не видно в списках, и знать об этом надо сразу.
    if !waiting.is_empty() {
        println!("it was waited on by: {}", waiting.join(", "));
    }
    Ok(())
}

/// Похожие тикеты. Ничего не меняет.
#[allow(clippy::too_many_arguments)]
async fn find(
    workspace: Option<String>,
    text: Option<String>,
    body: Option<String>,
    id: Option<String>,
    limit: Option<i64>,
    min_score: Option<f64>,
    json: bool,
) -> Result<()> {
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("no workspace given: pass -W, or set workspace in .ntkrc")?;
    if id.is_some() && (text.is_some() || body.is_some()) {
        anyhow::bail!("either --id or text: not both");
    }
    if id.is_none() && text.is_none() && body.is_none() {
        anyhow::bail!("give text to look for, or a ticket --id");
    }

    let mut req = serde_json::json!({});
    if let Some(v) = text { req["text"] = v.into(); }
    if let Some(v) = body { req["body"] = v.into(); }
    if let Some(v) = id { req["id"] = v.into(); }
    if let Some(v) = limit { req["limit"] = v.into(); }
    if let Some(v) = min_score { req["min_score"] = v.into(); }

    let v = api::Client::new(&cfg.url).similar(key, &ws, &req).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&v)?);
        return Ok(());
    }
    let hits = v.get("similar").and_then(|s| s.as_array()).cloned().unwrap_or_default();
    if hits.is_empty() {
        println!("nothing similar found");
        return Ok(());
    }
    println!("{}", api::render_similar(&hits));
    Ok(())
}

/// Сколько тикет стоит в текущем статусе, коротко: `3d`, `2h`, `—`.
///
/// Показывается в списке, потому что «сколько висит» — первое, что спрашивают о
/// чужой работе, и до сих пор ответить на это было нечем: поле база заполняла,
/// а наружу его не отдавали. Сутки и часы, без минут: точность здесь ничего не
/// добавляет, а колонку раздувает.
fn days_in_status(since: Option<&str>) -> String {
    let Some(raw) = since else { return "—".into() };
    let Ok(t) = raw.parse::<jiff::Timestamp>() else {
        // Postgres отдаёт `2026-09-10 12:18:15+00`, а не RFC 3339: пробел
        // вместо T и смещение без двоеточия. Чиним, а не молчим — иначе
        // колонка пустела бы у всех сразу и выглядела как «поля нет».
        let fixed = raw.replacen(' ', "T", 1);
        let fixed = if fixed.ends_with("+00") { format!("{}:00", fixed) } else { fixed };
        return match fixed.parse::<jiff::Timestamp>() {
            Ok(t) => span_short(t),
            Err(_) => "—".into(),
        };
    };
    span_short(t)
}

fn span_short(t: jiff::Timestamp) -> String {
    let secs = (jiff::Timestamp::now() - t).get_seconds();
    if secs < 0 {
        return "—".into();
    }
    let days = secs / 86_400;
    if days > 0 {
        format!("{days}d")
    } else {
        format!("{}h", secs / 3_600)
    }
}

async fn meta(workspace: Option<String>, json: bool) -> Result<()> {
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("no workspace given: pass -W, or set workspace in .ntkrc")?;

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

    println!("workspace: {ws}");
    let all = list("workspaces");
    if all.len() > 1 {
        println!("available:  {}", all.join(", "));
    }
    println!();
    println!("statuses:");
    if let Some(items) = m.get("statuses").and_then(|v| v.as_array()) {
        for s in items {
            let force = s.get("requires_force").and_then(|f| f.as_bool()).unwrap_or(false);
            println!(
                "  {:<12} {:<12} {}",
                s.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                s.get("group").and_then(|x| x.as_str()).unwrap_or(""),
                // Гард показывается словами: «нужен --force» понятнее, чем
                // requires_force=true, а знать это надо до правки, не после.
                if force { "editing requires --force" } else { "" }
            );
        }
    }
    println!();
    println!("priorities: {}", list("priorities").join(", "));
    println!("projects:   {}", list("projects").join(", "));
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
            println!("modules:    {}", names.join(", "));
        }
    }
    if let Some(people) = m.get("people").and_then(|v| v.as_array()) {
        let names: Vec<String> = people
            .iter()
            .map(|p| {
                let id = p.get("id").and_then(|x| x.as_str()).unwrap_or("");
                match p.get("kind").and_then(|x| x.as_str()) {
                    Some("agent") => format!("{id} (agent)"),
                    _ => id.to_string(),
                }
            })
            .collect();
        println!("people:     {}", names.join(", "));
    }
    Ok(())
}

/// Кто я и куда мне можно. Отвечает на первый вопрос после входа: что
/// подставлять в -W.
async fn whoami() -> Result<()> {
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let (user, ws) = api::Client::new(&cfg.url).me(key).await?;
    println!("you: {user}");
    if ws.is_empty() {
        println!("no workspaces — ask an administrator");
    } else {
        println!("workspaces: {}", ws.join(", "));
    }
    Ok(())
}

async fn modules(
    workspace: Option<String>,
    project: Option<String>,
    replace: bool,
    add: bool,
    stdin: bool,
    json: bool,
) -> Result<()> {
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("no workspace given: pass -W, or set workspace in .ntkrc")?;
    let client = api::Client::new(&cfg.url);

    if add {
        let project = project.clone().context("adding needs a project: -P")?;
        // Источник тот же, что у замены, но требование --stdin здесь мягче:
        // добавление ничего не убирает, поэтому забытый флаг не может стоить
        // реестра. Имена можно передать и через запятую.
        let list: Vec<String> = if stdin {
            let mut text = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
                .context("could not read the list from standard input")?;
            text.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(String::from)
                .collect()
        } else {
            anyhow::bail!("name the modules: --add --stdin (one name per line)");
        };
        if list.is_empty() {
            anyhow::bail!("the list is empty: nothing to add");
        }
        let r = client.add_modules(key, &ws, &project, &list).await?;
        if json {
            println!("{}", serde_json::to_string_pretty(&r)?);
            return Ok(());
        }
        let names = |k: &str| -> Vec<String> {
            r.get(k).and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default()
        };
        for (label, key) in [("filed", "added"), ("brought back from the archive", "restored")] {
            let v = names(key);
            if !v.is_empty() { println!("  {label}: {}", v.join(", ")); }
        }
        // Сказать это вслух важнее, чем кажется: операцию берут именно потому,
        // что она ничего не убирает, и проверяющий должен видеть подтверждение,
        // а не выводить его из тишины.
        println!("  nothing was taken out of the live set");
        return Ok(());
    }

    if replace {
        let project = project.context("replacing the list needs a project: -P")?;
        if !stdin {
            // Замена набора необратима для тех, кто исчезнет, поэтому источник
            // называется явно. Молчаливое чтение stdin означало бы, что
            // забытый флаг стирает реестр.
            anyhow::bail!("pass --stdin: the list is read from standard input, one name per line");
        }
        let mut text = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut text)
            .context("could not read the list from standard input")?;
        let list: Vec<String> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(String::from)
            .collect();
        if list.is_empty() {
            anyhow::bail!("the list is empty: replacing would wipe the project's whole registry; if that is the intent, send at least one module");
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
        println!("{project}: {} in total", r.get("total").and_then(|t| t.as_i64()).unwrap_or(0));
        for (label, key) in [("added", "added"), ("to the archive", "archived"),
                             ("deleted", "deleted"), ("brought back", "restored")] {
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
        println!("no modules");
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
            if archived { "archived" } else { "" }
        );
    }
    Ok(())
}

#[cfg(test)]
mod describe_tests {
    use super::{api::Filters, describe};

    fn with_module(m: Option<&str>) -> Filters {
        Filters {
            status: Some("open".into()),
            tag: None,
            title: None,
            assignee: None,
            project: Some("ntk".into()),
            module: m.map(str::to_string),
            strict: false,
            all: true,
            stale: None,
        }
    }

    // describe() — не украшение вывода: этой строкой различаются СЕАНСЫ обхода
    // (walk_id хранится под ключом "воркспейс|описание"). Пока модуля в ней не
    // было, два обхода с разным --module делили один курсор, и второй
    // продолжал с того места, где кончился первый, — молча пропуская тикеты.
    #[test]
    fn module_makes_the_walk_session_distinct() {
        let a = describe(&with_module(Some("server/db")));
        let b = describe(&with_module(Some("server/api")));
        let none = describe(&with_module(None));
        assert_ne!(a, b, "обходы по разным модулям обязаны разойтись: {a}");
        assert_ne!(a, none, "обход по модулю не равен обходу без модуля: {a}");
        assert!(a.contains("server/db"), "модуль не назван: {a}");
    }
}

/// Жизненный цикл проекта.
///
/// Ровно одно действие за вызов: перенос и архивирование по отдельности
/// обратимы по-разному, и склеивать их в один шаг значит лишить человека
/// возможности остановиться между ними и посмотреть, что получилось.
async fn projects(
    workspace: Option<String>,
    archive: Option<String>,
    unarchive: Option<String>,
    move_from: Option<String>,
    to: Option<String>,
    json: bool,
) -> Result<()> {
    let cfg = config::load()?;
    let key = config::require_key(&cfg)?;
    let ws = workspace
        .or_else(config::workspace_from_rc)
        .context("no workspace given: pass -W, or set workspace in .ntkrc")?;
    let c = api::Client::new(&cfg.url);

    let asked = [archive.is_some(), unarchive.is_some(), move_from.is_some()]
        .iter()
        .filter(|x| **x)
        .count();
    if asked > 1 {
        anyhow::bail!("one action at a time: --archive, --unarchive or --move");
    }

    if let Some(from) = move_from {
        let to = to.context("--move needs --to: name the target project")?;
        let v = c.move_project(&key, &ws, &from, &to).await?;
        if json {
            println!("{v}");
        } else {
            let n = v.get("moved").and_then(|x| x.as_i64()).unwrap_or(0);
            println!("tickets moved: {n} — {from} → {to}");
            // Сказать это обязательно: иначе несовпадение префикса выглядит
            // поломкой, и кто-нибудь пойдёт «чинить» идентификаторы.
            println!("identifiers did not change: the tickets keep the old {from}- prefix");
            println!("retire the emptied name: ntk projects --archive {from} -W {ws}");
        }
        return Ok(());
    }

    if let Some(id) = archive.or(unarchive.clone()) {
        let want_archived = unarchive.is_none();
        let v = c.set_project_archived(&key, &ws, &id, want_archived).await?;
        if json {
            println!("{v}");
        } else if want_archived {
            println!("{id} is retired from the choices; the tickets already filed stay where they are");
        } else {
            println!("{id} is available for selection again");
        }
        return Ok(());
    }

    let m = c.meta(&key, &ws).await?;
    if json {
        println!("{}", m.get("projects").cloned().unwrap_or(serde_json::Value::Null));
        return Ok(());
    }
    match m.get("projects").and_then(|p| p.as_array()) {
        Some(ps) if !ps.is_empty() => {
            for p in ps {
                println!("{}", p.as_str().unwrap_or_default());
            }
        }
        _ => println!("no projects"),
    }
    Ok(())
}
