//! MCP по Streamable HTTP: Claude Desktop подключается по одной ссылке.
//!
//! Тот же набор инструментов, что у локального сервера в клиенте, но без
//! бинаря на машине человека. Юрист вставляет адрес в «Add custom connector»,
//! проходит Google — и видит свои тикеты.
//!
//! Инструменты НЕ реализованы здесь заново: каждый зовёт тот самый обработчик,
//! что обслуживает REST. Вторая реализация тех же вызовов — это второе место,
//! где живёт гард «тикет уже подобран» и захват через SKIP LOCKED, и она
//! разошлась бы с первым молча. Поэтому здесь только разбор JSON-RPC и
//! перекладывание параметров.

use std::sync::Arc;

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures_util::StreamExt;
use serde_json::{json, Value};

use crate::{auth, mcp_oauth, write, App};

/// Кому уже сказали «перечитай список инструментов».
///
/// Держится в памяти процесса намеренно: набор инструментов меняется ровно при
/// выкатке, а выкатка перезапускает процесс. Значит после каждой выкатки память
/// пуста, и первый же вызов каждого клиента получает уведомление — ровно то,
/// что нужно, и без единой строки в базе.
static TOLD: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::OnceLock::new();

fn told_already(who: &str) -> bool {
    let set = TOLD.get_or_init(Default::default);
    let mut g = set.lock().unwrap_or_else(|e| e.into_inner());
    // Дальше набор не растёт бесконечно: процесс живёт до следующей выкатки, а
    // клиентов у нас десятки, не миллионы.
    !g.insert(who.to_string())
}

/// Версия протокола, о которой договариваемся. Клиент присылает свою в
/// `initialize`; если она новее, мы отвечаем своей — так требует спецификация.
const PROTOCOL: &str = "2025-06-18";

/// Tool descriptions, taken from `ntk_core::tools` — the same place the
/// terminal and the local MCP inside the client take them from.
///
/// There used to be a separate list here, and the comment above it said it
/// matched "the local server in the client word for word". It did not: the
/// client had sixteen tools, this had thirteen — the three module tools were
/// absent over HTTP entirely — and close, deps, rm, meta and walk had drifted
/// apart in wording. A promise in a comment is checked by nothing; the
/// catalogue is checked by a test.
fn tools() -> Value {
    ntk_core::tools::manifest()
}

/// Отпечаток набора инструментов. Входит в идентификатор сессии.
///
/// Сессия, выданная под другой набор, — устаревшая. Спецификация разрешает
/// прекратить её в любой момент и требует отвечать на её запросы 404, а клиент
/// ОБЯЗАН после этого начать новую и заново спросить список. Это единственный
/// рычаг, которым сервер роняет соединение со своей стороны, — ровно то, что
/// произошло у соседнего сервера, после чего клиент сам увидел его новый состав.
fn tools_tag() -> &'static str {
    static TAG: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    TAG.get_or_init(|| {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(tools().to_string().as_bytes()))[..8].to_string()
    })
}

fn rnd() -> String {
    use rand::Rng;
    const A: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut r = rand::thread_rng();
    (0..16).map(|_| A[r.gen_range(0..A.len())] as char).collect()
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k)?.as_str().map(str::to_owned)
}

/// Предел ответа, который берём в память. 500 тикетов с телами — это единицы
/// мегабайт; прежние 2 МБ на них не хватало.
const MAX_BODY: usize = 24 * 1024 * 1024;

/// Ответ обработчика → текст для модели.
///
/// Код состояния не выбрасываем: отказ «тикет уже взят» — это осмысленный
/// ответ, который модель должна прочитать, а не молчание.
///
/// А вот ошибку чтения тела выбрасывать нельзя тем более. Здесь стоял
/// `unwrap_or_default()`, и ответ длиннее предела превращался в пустую строку
/// с пометкой «успех»: `ntk_ls` с limit=500 отвечал пустотой, а с limit=50
/// работал, и понять, тикетов нет или ответ не поместился, было нельзя.
/// Молчаливая пустота вместо отказа — худший из возможных ответов.
async fn body_text(r: Response) -> (bool, String) {
    let ok = r.status().is_success();
    match axum::body::to_bytes(r.into_body(), MAX_BODY).await {
        Ok(bytes) => (ok, String::from_utf8_lossy(&bytes).into_owned()),
        Err(e) => (
            false,
            format!(
                "ответ не поместился в {} МБ ({e}). Возьмите меньше за раз: \
                 уменьшите limit или отберите по status и tag",
                MAX_BODY / (1024 * 1024)
            ),
        ),
    }
}

fn hdrs(token: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    if let Ok(v) = axum::http::HeaderValue::from_str(&format!("Bearer {token}")) {
        h.insert(axum::http::header::AUTHORIZATION, v);
    }
    h
}

async fn call_tool(app: &Arc<App>, token: &str, name: &str, args: &Value) -> (bool, String) {
    let st = State(app.clone());
    let h = hdrs(token);
    let ws = s(args, "workspace");
    let id = s(args, "id").unwrap_or_default();

    match name {
        "ntk_whoami" => body_text(crate::whoami(st, h).await).await,

        "ntk_ls" => {
            let mut q = vec![];
            if let Some(w) = &ws { q.push(format!("workspace={}", urlencoding::encode(w))); }
            if let Some(v) = s(args, "status") { q.push(format!("status={}", urlencoding::encode(&v))); }
            if let Some(v) = args.get("limit").and_then(|v| v.as_i64()) { q.push(format!("limit={v}")); }
            if let Some(v) = args.get("offset").and_then(|v| v.as_i64()) { q.push(format!("offset={v}")); }
            if args.get("all").and_then(|v| v.as_bool()).unwrap_or(false) { q.push("all=true".into()); }
            if let Some(v) = s(args, "tag") { q.push(format!("tag={}", urlencoding::encode(&v))); }
            if let Some(v) = s(args, "title") { q.push(format!("title={}", urlencoding::encode(&v))); }
            // module was missing here: the handler accepts it but the
            // dispatcher never forwarded it — so filtering by module was
            // SILENTLY ignored and everything came back. That is worse than a
            // refusal: a refusal is visible.
            for k in ["assignee", "project", "module"] {
                if let Some(v) = s(args, k) { q.push(format!("{k}={}", urlencoding::encode(&v))); }
            }
            if args.get("strict").and_then(|v| v.as_bool()).unwrap_or(false) { q.push("strict=true".into()); }
            if args.get("count").and_then(|v| v.as_bool()).unwrap_or(false) { q.push("count=true".into()); }
            match Query::try_from_uri(&format!("/?{}", q.join("&")).parse().unwrap()) {
                Ok(Query(qq)) => body_text(crate::tickets(st, h, Query(qq)).await).await,
                Err(e) => (false, e.to_string()),
            }
        }

        "ntk_show" => {
            let uri = format!("/?workspace={}", urlencoding::encode(ws.as_deref().unwrap_or("")));
            match Query::try_from_uri(&uri.parse().unwrap()) {
                Ok(Query(qq)) => body_text(crate::ticket_one(st, h, Path(id), Query(qq)).await).await,
                Err(e) => (false, e.to_string()),
            }
        }

        "ntk_next" => {
            let mut uri = format!("/?workspace={}", urlencoding::encode(ws.as_deref().unwrap_or("")));
            for k in ["prefer", "tag", "project", "module", "assignee"] {
                if let Some(v) = s(args, k) { uri.push_str(&format!("&{k}={}", urlencoding::encode(&v))); }
            }
            for k in ["strict", "has_module", "dry_run"] {
                if args.get(k).and_then(|v| v.as_bool()).unwrap_or(false) { uri.push_str(&format!("&{k}=true")); }
            }
            match Query::try_from_uri(&uri.parse().unwrap()) {
                Ok(Query(qq)) => body_text(write::next(st, h, Query(qq)).await).await,
                Err(e) => (false, e.to_string()),
            }
        }

        "ntk_start" => {
            let uri = format!("/?workspace={}", urlencoding::encode(ws.as_deref().unwrap_or("")));
            match Query::try_from_uri(&uri.parse().unwrap()) {
                Ok(Query(qq)) => body_text(write::start(st, h, Path(id), Query(qq)).await).await,
                Err(e) => (false, e.to_string()),
            }
        }

                "ntk_walk" => {
            let mut q = vec![];
            if let Some(w) = &ws { q.push(format!("workspace={}", urlencoding::encode(w))); }
            if let Some(v) = s(args, "walk_id") { q.push(format!("walk_id={}", urlencoding::encode(&v))); }
            for k in ["status", "tag", "title", "assignee", "project", "module"] {
                if let Some(v) = s(args, k) { q.push(format!("{k}={}", urlencoding::encode(&v))); }
            }
            for k in ["strict", "all", "reset"] {
                if args.get(k).and_then(|v| v.as_bool()).unwrap_or(false) { q.push(format!("{k}=true")); }
            }
            match Query::try_from_uri(&format!("/?{}", q.join("&")).parse().unwrap()) {
                Ok(Query(qq)) => body_text(crate::walk::step(st, h, Query(qq)).await).await,
                Err(e) => (false, e.to_string()),
            }
        }

        "ntk_create" => {
            let mut p = json!({"title": s(args, "title").unwrap_or_default()});
            if let Some(v) = s(args, "project") { p["project"] = json!(v); }
            if let Some(w) = &ws { p["workspace"] = json!(w); }
            for k in ["body", "assignee", "module", "priority", "status", "type"] {
                if let Some(v) = s(args, k) { p[k] = json!(v); }
            }
            if args.get("skip_search").and_then(|v| v.as_bool()).unwrap_or(false) {
                p["skip_search"] = json!(true);
            }
            if let Some(t) = args.get("tags") { p["tags"] = t.clone(); }
            if let Some(d) = args.get("deps") { p["deps"] = d.clone(); }
            match serde_json::from_value(p) {
                Ok(parsed) => body_text(write::create(st, h, Json(parsed)).await).await,
                Err(e) => (false, e.to_string()),
            }
        }

"ntk_update" | "ntk_close" | "ntk_tag" => {
            let mut p = json!({});
            if let Some(w) = &ws { p["workspace"] = json!(w); }
            if args.get("force").and_then(|v| v.as_bool()).unwrap_or(false) { p["force"] = json!(true); }
            match name {
                "ntk_close" => { p["status"] = json!("done"); }
                "ntk_tag" => {
                    let Some(e) = args.get("edits") else { return (false, "не указаны edits".into()) };
                    p["tag_edits"] = e.clone();
                }
                _ => {
                    if args.get("body").is_some() && args.get("body_append").is_some() {
                        return (false, "body и body_append вместе не принимаются: либо заменить тело, либо дописать".into());
                    }
                    // module и project здесь же, а не отдельным вызовом: сервер
                    // меняет всё одной транзакцией, а два вызова оставляют тикет
                    // видимым наполовину изменённым. Реестр модулей существовал,
                    // а назвать модуль по MCP было нечем.
                    for k in ["status", "title", "body", "body_append", "assignee",
                              "module", "project", "priority", "type", "due"] {
                        if let Some(v) = s(args, k) { p[k] = json!(v); }
                    }
                    if let Some(t) = args.get("tag_edits") { p["tag_edits"] = t.clone(); }
                    // Dependencies. The server handled them and the local
                    // client offered them, but the HTTP list never mentioned
                    // them — so through the web client there was NOTHING to fix
                    // links with after creation, and it looked like "ntk cannot
                    // do that".
                    for k in ["dep_edits", "dep_set"] {
                        if let Some(d) = args.get(k) { p[k] = d.clone(); }
                    }
                    if let Some(edits) = p.get("dep_edits").and_then(|v| v.as_array()) {
                        for e in edits {
                            let t = e.as_str().unwrap_or("");
                            if !t.starts_with('+') && !t.starts_with('-') {
                                return (false, format!("dependency \u{00ab}{t}\u{00bb} has no sign: use + (start waiting) or - (stop)"));
                            }
                        }
                    }
                    if p.get("dep_set").and_then(|v| v.as_array()).is_some_and(|set| {
                        set.iter().any(|e| {
                            let t = e.as_str().unwrap_or("");
                            t.starts_with('+') || t.starts_with('-')
                        })
                    }) {
                        return (false, "dep_set replaces the whole set — signs do not belong here; use dep_edits to edit".into());
                    }
                }
            }
            // Знак у каждой правки обязателен, и проверяем его ДО записи:
            // «добавить», понятое как «заменить всё», стирает историю пометок
            // без единой ошибки.
            if let Some(edits) = p.get("tag_edits").and_then(|v| v.as_array()) {
                for e in edits {
                    let t = e.as_str().unwrap_or("");
                    if !t.starts_with('+') && !t.starts_with('-') {
                        return (false, format!("тег «{t}» без знака: нужен + или -"));
                    }
                }
            }
            let parsed = match serde_json::from_value(p) {
                Ok(v) => v,
                Err(e) => return (false, e.to_string()),
            };
            body_text(write::patch(st, h, Path(id), Json(parsed)).await).await
        }

        "ntk_deps" | "ntk_rm" | "ntk_meta" => {
            let uri = format!("/?workspace={}", urlencoding::encode(ws.as_deref().unwrap_or("")));
            let Ok(Query(qq)) = Query::try_from_uri(&uri.parse().unwrap()) else {
                return (false, "не указан воркспейс".into());
            };
            match name {
                "ntk_deps" => body_text(write::deps(st, h, Path(id), Query(qq)).await).await,
                "ntk_rm" => body_text(write::remove(st, h, Path(id), Query(qq)).await).await,
                _ => body_text(write::meta(st, h, Query(qq)).await).await,
            }
        }

        "ntk_similar" => {
            let mut q = json!({});
            if let Some(w) = &ws {
                q["workspace"] = json!(w);
            }
            for k in ["title", "body", "id"] {
                if let Some(v) = s(args, k) {
                    q[k] = json!(v);
                }
            }
            if let Some(v) = args.get("limit").and_then(|v| v.as_i64()) {
                q["limit"] = json!(v);
            }
            if let Some(v) = args.get("min_score").and_then(|v| v.as_f64()) {
                q["min_score"] = json!(v);
            }
            match serde_json::from_value(q) {
                Ok(parsed) => body_text(write::similar(st, h, Json(parsed)).await).await,
                Err(e) => (false, e.to_string()),
            }
        }

        // Modules. Over HTTP these did not exist at all: the registry was
        // there, the local client used it, and through the web client there was
        // no way to name a module — with no way to understand why "the same
        // thing" worked in one place and not in the other.
        "ntk_modules" => {
            let uri = format!("/?workspace={}", urlencoding::encode(ws.as_deref().unwrap_or("")));
            let Ok(Query(qq)) = Query::try_from_uri(&uri.parse().unwrap()) else {
                return (false, "no workspace given".into());
            };
            let (ok, text) = body_text(write::meta(st, h, Query(qq)).await).await;
            if !ok {
                return (ok, text);
            }
            let want = s(args, "project");
            match serde_json::from_str::<Value>(&text) {
                Ok(v) => {
                    let rows: Vec<Value> = v
                        .get("modules")
                        .and_then(|m| m.as_array())
                        .map(|all| {
                            all.iter()
                                .filter(|x| match want.as_deref() {
                                    None => true,
                                    Some(p) => x.get("project").and_then(|v| v.as_str()) == Some(p),
                                })
                                .cloned()
                                .collect()
                        })
                        .unwrap_or_default();
                    (true, Value::Array(rows).to_string())
                }
                Err(e) => (false, format!("the directory did not parse: {e}")),
            }
        }

        "ntk_modules_add" | "ntk_modules_replace" => {
            let Some(project) = s(args, "project") else {
                return (false, "no project given".into());
            };
            let key = if name == "ntk_modules_add" { "add" } else { "modules" };
            let Some(list) = args.get(key).and_then(|v| v.as_array()) else {
                return (false, format!("no {key} given"));
            };
            if list.is_empty() {
                // An empty list on replace would wipe the project's registry
                // whole, and over MCP that costs exactly one missing argument.
                // Hence a refusal.
                return (
                    false,
                    if name == "ntk_modules_add" {
                        "name at least one module".into()
                    } else {
                        "an empty list would wipe the project's whole registry: send at least one module"
                            .to_string()
                    },
                );
            }
            let mut p = json!({ key: Value::Array(list.clone()) });
            if let Some(w) = &ws {
                p["workspace"] = json!(w);
            }
            if name == "ntk_modules_add" {
                match serde_json::from_value(p) {
                    Ok(parsed) => {
                        body_text(write::add_modules(st, h, Path(project), Json(parsed)).await).await
                    }
                    Err(e) => (false, e.to_string()),
                }
            } else {
                match serde_json::from_value(p) {
                    Ok(parsed) => {
                        body_text(write::set_modules(st, h, Path(project), Json(parsed)).await).await
                    }
                    Err(e) => (false, e.to_string()),
                }
            }
        }

        other => (false, format!("инструмента {other} нет")),
    }
}

/// Одна точка MCP. Принимает JSON-RPC, отвечает JSON-ом.
///
/// Ответ идёт обычным JSON, а не потоком событий: спецификация Streamable HTTP
/// это разрешает, а поток нужен серверам, которые шлют уведомления по ходу
/// вызова. У нас каждый инструмент — один запрос к базе и один ответ; поток
/// добавил бы состояние сессии, которое нечем наполнить.
pub async fn endpoint(State(app): State<Arc<App>>, headers: HeaderMap, body: String) -> Response {
    let base = app.cfg.public_url.clone();
    let msg: Value = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => return rpc_error(Value::Null, -32700, &format!("разбор не удался: {e}")),
    };
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or_default();

    // Сессия под прежний набор инструментов прекращается: клиент обязан начать
    // новую и заново спросить список. Так он узнаёт об изменениях без единого
    // действия человека — и без надежды на поток по GET, который он открывать
    // не обязан и не открывает.
    if method != "initialize" {
        if let Some(sid) = headers.get("mcp-session-id").and_then(|v| v.to_str().ok()) {
            if !sid.starts_with(tools_tag()) {
                tracing::info!(sid, "mcp: сессия под прежний набор — прекращаю");
                return (
                    StatusCode::NOT_FOUND,
                    Json(json!({"error": "сессия прекращена: набор инструментов изменился"})),
                )
                    .into_response();
            }
        }
    }

    // Ключ спрашиваем на КАЖДОМ запросе, включая initialize.
    //
    // Сначала он требовался только на вызове инструмента, а рукопожатие
    // проходило без него. Claude Desktop при добавлении коннектора щупает
    // точку без ключа и по отсутствию отказа решает, что вход не нужен: в
    // списке способов авторизации подсвечивалось «None — Detected», то есть
    // подключиться предлагалось вообще без входа. Спецификация требует того же
    // прямо: заголовок обязателен в каждом запросе, даже внутри одной сессии.
    let Some(token) = auth::bearer(&headers) else {
        return mcp_oauth::unauthorized(&base, "нужен заголовок Authorization: Bearer");
    };
    if let Err(e) = check_audience(&app, token, &base).await {
        return mcp_oauth::unauthorized(&base, &e.to_string());
    }
    let token = token.to_string();

    // Запись того, что спрашивает клиент. Без неё «Claude не видит новые
    // инструменты» неотличимо от «Claude их не спрашивал»: первое чинится на
    // сервере, второе — только переподключением, и перепутать их дорого.
    tracing::info!(
        method,
        client = msg
            .get("params")
            .and_then(|p| p.get("clientInfo"))
            .and_then(|c| c.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("-"),
        "mcp"
    );

    // Уведомления ответа не требуют.
    if msg.get("id").is_none() {
        return (StatusCode::ACCEPTED, Body::empty()).into_response();
    }

    match method {
        "initialize" => {
            let asked = msg
                .get("params")
                .and_then(|p| p.get("protocolVersion"))
                .and_then(|v| v.as_str())
                .unwrap_or(PROTOCOL);
            let agreed = if asked > PROTOCOL { PROTOCOL } else { asked };
            // Идентификатор сессии несёт отпечаток набора инструментов: по нему
            // видно, под какой набор она заведена, и старую можно прекратить.
            let sid = format!("{}-{}", tools_tag(), rnd());
            let mut r = rpc_ok(id, json!({
                "protocolVersion": agreed,
                // listChanged: true — обещание слать уведомление, когда набор
                // инструментов изменится. Без него клиент не обязан слушать, и
                // перечитывать список ему незачем.
                "capabilities": {"tools": {"listChanged": true}},
                "serverInfo": {"name": "ntk", "version": env!("CARGO_PKG_VERSION")}
            }));
            if let Ok(v) = axum::http::HeaderValue::from_str(&sid) {
                r.headers_mut().insert("mcp-session-id", v);
            }
            r
        }
        "ping" => rpc_ok(id, json!({})),
        "tools/list" => rpc_ok(id, json!({"tools": tools()})),
        "tools/call" => {
            let params = msg.get("params").cloned().unwrap_or(json!({}));
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or_default();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            let (ok, text) = call_tool(&app, &token, name, &args).await;
            let result = json!({
                "content": [{"type": "text", "text": text}],
                "isError": !ok
            });
            // Кого считаем «этим клиентом»: сессию, если он её ведёт, иначе сам
            // токен. Токен принадлежит человеку, поэтому хуже всего, что может
            // случиться, — одно лишнее перечитывание списка на человека.
            let who = headers
                .get("mcp-session-id")
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
                .unwrap_or_else(|| mcp_oauth::hash(&token));
            if told_already(&who) {
                rpc_ok(id, result)
            } else {
                tracing::info!("mcp: сказали перечитать список инструментов");
                rpc_ok_with_notice(id, result)
            }
        }
        other => rpc_error(id, -32601, &format!("метод {other} не поддерживается")),
    }
}

/// Токен обязан быть выпущен ДЛЯ НАС.
///
/// Принять токен, выпущенный для другого ресурса, значит доверить чужому
/// серверу авторизации решать, кто здесь свой. Спецификация называет это прямо
/// запрещённым, и не зря: иначе достаточно одной дружелюбной службы, чтобы
/// выдать пропуск в чужую очередь тикетов.
async fn check_audience(app: &Arc<App>, token: &str, base: &str) -> anyhow::Result<()> {
    if !token.starts_with(auth::OAUTH_ACCESS_PREFIX) {
        return Ok(()); // обычный ключ ntk: он и выдан нами, и живёт только здесь
    }
    let c = app.pool.get().await?;
    let row = c
        .query_opt(
            "select resource from core.oauth_tokens
              where token_hash = $1 and kind = 'access' and revoked_at is null
                and (expires_at is null or expires_at > now())",
            &[&mcp_oauth::hash(token)],
        )
        .await?;
    let Some(r) = row else {
        anyhow::bail!("токен неизвестен или просрочен");
    };
    if let Some(res) = r.get::<_, Option<String>>(0) {
        let want = mcp_oauth::resource_url(base);
        let norm = |s: &str| s.trim_end_matches('/').to_ascii_lowercase();
        if norm(&res) != norm(&want) && norm(&res) != norm(base) {
            anyhow::bail!("токен выпущен для другого ресурса: {res}");
        }
    }
    Ok(())
}

fn rpc_ok(id: Value, result: Value) -> Response {
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

/// Ответ потоком, с уведомлением ПЕРЕД самим ответом.
///
/// Единственный способ достучаться до клиента, который не открывает поток по
/// GET, — а он не обязан и не открывает: за час на точку пришёл ровно один GET,
/// и тот мой, курлом. Спецификация это разрешает прямо: «сервер МОЖЕТ послать
/// запросы и уведомления перед ответом».
///
/// Без этого клиент, подключившийся вчера, вчерашние инструменты и видит:
/// сервер отдаёт тринадцать, а он пользуется двенадцатью и о новых параметрах
/// не знает. Переподключать коннектор всей команде на каждую выкатку — не
/// решение, а перекладывание своей работы на людей.
fn rpc_ok_with_notice(id: Value, result: Value) -> Response {
    let notice = json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}).to_string();
    let answer = json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
    let events = futures_util::stream::iter(vec![
        Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().data(notice)),
        Ok(axum::response::sse::Event::default().data(answer)),
    ]);
    // Поток закрывается сразу после ответа, как требует спецификация.
    axum::response::Sse::new(events).into_response()
}

fn rpc_error(id: Value, code: i64, message: &str) -> Response {
    Json(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})).into_response()
}

/// GET на ту же точку — поток событий от сервера к клиенту.
///
/// Нужен ровно ради одного: сказать «перечитай список инструментов». В MCP это
/// единственный способ обновить набор посреди сессии — номера версии у описаний
/// нет, а версия сервера объявляется один раз при подключении и больше не
/// пересматривается. Без потока клиент, подключившийся вчера, вчерашние
/// инструменты и видит: новый параметр отбора для него не существует, и узнать
/// о нём неоткуда.
///
/// Уведомление уходит СРАЗУ при открытии потока. Выкатка перезапускает службу,
/// открытые потоки рвутся, клиент подключается заново — и тем самым перечитывает
/// список. Хранить, кто какую версию видел, для этого не нужно: лишний
/// перезапрос стоит одного запроса, а пропущенный — незнания об инструменте.
pub async fn endpoint_get(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    let base = app.cfg.public_url.clone();
    tracing::info!("mcp: открыт поток уведомлений");
    let Some(token) = auth::bearer(&headers) else {
        return mcp_oauth::unauthorized(&base, "нужен заголовок Authorization: Bearer");
    };
    if let Err(e) = check_audience(&app, token, &base).await {
        return mcp_oauth::unauthorized(&base, &e.to_string());
    }

    let hello = json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}).to_string();
    let stream = futures_util::stream::once(async move {
        Ok::<_, std::convert::Infallible>(axum::response::sse::Event::default().data(hello))
    })
    .chain(futures_util::stream::pending());

    axum::response::Sse::new(stream)
        // Тишину в потоке посредники обрывают молча. Пульс раз в 15 секунд
        // держит соединение живым и не даёт принять разрыв за отсутствие
        // новостей.
        .keep_alive(axum::response::sse::KeepAlive::new().interval(std::time::Duration::from_secs(15)))
        .into_response()
}

#[cfg(test)]
mod tests {
    /// Everything the list advertises must be callable here.
    ///
    /// The drift ran exactly this way, only in the other direction: three
    /// module tools lived in the client and were absent here, so through the
    /// web client there was no way to name a module. Now the list is shared,
    /// which makes it possible to advertise a tool and forget it in the
    /// dispatcher — and that is precisely what this checks.
    ///
    /// It checks the file's text rather than making a call: the dispatcher is a
    /// `match` on a name inside an async function that carries the service
    /// state, and standing it up in a test would cost a fake pool, a fake key
    /// and fake headers. A cheap check that catches the real mistake beats an
    /// expensive one that never gets written.
    #[test]
    fn every_listed_tool_has_a_branch_in_the_dispatcher() {
        let src = include_str!("mcp_http.rs");
        let body = src
            .split_once("async fn call_tool(")
            .expect("the dispatcher was renamed — fix this test")
            .1;
        for t in ntk_core::tools::ALL {
            let arm = format!("\"{}\"", t.name);
            assert!(
                body.contains(&arm),
                "{} is in the catalogue but has no branch in the dispatcher",
                t.name
            );
        }
    }

    /// The list comes from the catalogue and from nowhere else.
    #[test]
    fn the_list_is_the_catalogue() {
        assert_eq!(super::tools(), ntk_core::tools::manifest());
        let n = super::tools().as_array().map(|a| a.len()).unwrap_or(0);
        assert_eq!(n, ntk_core::tools::ALL.len());
    }
}
