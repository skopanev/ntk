//! MCP-сервер: те же команды, но для модели, а не для человека.
//!
//! Живёт в клиентском бинаре намеренно. Отдельный сервер означал бы вторую
//! реализацию тех же вызовов и второе место, где они разъезжаются; здесь
//! инструменты ходят через тот же `api::Client`, что и команды CLI.

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{CallToolResult, Content, Implementation, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler,
};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::{api, config};

#[derive(Clone)]
pub struct Ntk {
    tool_router: ToolRouter<Ntk>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct LsArgs {
    /// Воркспейс. Обязателен: значения по умолчанию нет.
    pub workspace: String,
    /// Статус: open, in_progress, to_test, to_review, reviewed, blocked, done.
    pub status: Option<String>,
    /// Сколько вернуть. По умолчанию 50, потолок 500.
    pub limit: Option<i64>,
    /// Сколько пропустить — следующая страница.
    pub offset: Option<i64>,
    /// Показать тикеты всех, а не только свои.
    pub all: Option<bool>,
    /// Отбор по тегам через запятую. Все перечисленные должны быть на тикете.
    /// По умолчанию по вхождению: "infra" находит и "initiative:infra".
    pub tag: Option<String>,
    /// Тег должен совпасть целиком, а не войти частью.
    pub strict: Option<bool>,
    /// Отбор по исполнителю. Сильнее умолчания «мои».
    pub assignee: Option<String>,
    /// Отбор по проекту.
    pub project: Option<String>,
    /// Отбор по модулю — единице работы внутри проекта.
    pub module: Option<String>,
    /// Отбор по заголовку: вхождение подстроки, регистр не важен.
    pub title: Option<String>,
    /// Вернуть только ЧИСЛО подходящих, без самих тикетов. Потолок выдачи в
    /// 500 счёту не мешает: считает база.
    pub count: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShowArgs {
    /// Идентификатор вида proj-xxxxxxxxxx. Регистр не важен.
    pub id: String,
    pub workspace: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct IdArgs {
    /// Идентификатор вида proj-xxxxxxxxxx. Регистр не важен.
    pub id: String,
    pub workspace: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WsArgs {
    pub workspace: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ModulesArgs {
    pub workspace: String,
    /// Проект. Без него — модули всех проектов воркспейса.
    pub project: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ModulesReplaceArgs {
    pub workspace: String,
    pub project: String,
    /// ПОЛНЫЙ список модулей проекта. Чего в нём нет — уйдёт из действующих.
    pub modules: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ModulesAddArgs {
    pub workspace: String,
    pub project: String,
    /// Имена, которые надо завести. Реестр ДОПОЛНЯЕТСЯ: ничего не уходит из
    /// действующих, в отличие от замены.
    pub add: Vec<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CloseArgs {
    pub id: String,
    pub workspace: String,
    /// Снять гард «тикет уже подобран».
    pub force: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct TagArgs {
    pub id: String,
    pub workspace: String,
    /// Правки тегов: каждый со знаком, например ["+alpha","-legacy"].
    /// Знак обязателен: тег без него отвергается, чтобы «добавить» не оказалось
    /// «заменить всё».
    pub edits: Vec<String>,
    /// Снять гард «тикет уже подобран». Нужен, пока пометка тикета в работе
    /// считается его правкой.
    pub force: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct NextArgs {
    pub workspace: String,
    /// Теги в порядке предпочтения, через запятую. ПОРЯДОК, а не фильтр:
    /// если по ним ничего нет, будет взят любой подходящий тикет.
    pub prefer: Option<String>,
    /// Отбор по тегам через запятую. В отличие от prefer ИСКЛЮЧАЕТ: не
    /// подошло — не выдаётся вовсе.
    pub tag: Option<String>,
    /// Тег должен совпасть целиком, а не войти частью.
    pub strict: Option<bool>,
    pub project: Option<String>,
    /// Отбор по конкретному модулю.
    pub module: Option<String>,
    /// Любой ДЕЙСТВУЮЩИЙ модуль вместо конкретного имени: «единица работы
    /// назначена». Архивный не считается.
    pub has_module: Option<bool>,
    pub assignee: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateArgs {
    pub workspace: String,
    /// Проект — префикс идентификатора тикета.
    pub project: String,
    pub title: String,
    /// Тело в markdown.
    pub body: Option<String>,
    /// Кому. Короткий идентификатор из core.users, например sk.
    pub assignee: Option<String>,
    pub tags: Option<Vec<String>>,
    /// Модуль — единица работы внутри проекта. Допустимые перечисляет ntk_meta:
    /// угадывать их не нужно и не следует. Реестр модулей есть, а назвать
    /// модуль по MCP было НЕЧЕМ — приходилось заводить тикет без него.
    pub module: Option<String>,
    pub priority: Option<String>,
    /// Начальный статус. Без него сервер ставит первый из группы todo.
    pub status: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// Идентификаторы тикетов, которых этот ждёт.
    pub deps: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct UpdateArgs {
    pub workspace: String,
    pub id: String,
    /// Новый статус. Правка тикета вне группы todo требует force.
    pub status: Option<String>,
    pub title: Option<String>,
    /// Тело целиком. ЗАМЕНЯЕТ прежнее. Чтобы добавить строку, нужен body_append.
    pub body: Option<String>,
    /// Дописать в конец тела, не трогая написанное.
    ///
    /// Склейка идёт в базе одним UPDATE: вычитать тело, склеить у себя и
    /// записать целиком — это и лишний повод стереть чужое, и гонка.
    pub body_append: Option<String>,
    pub assignee: Option<String>,
    /// Правки тегов, каждая со знаком: ["+alpha","-legacy"].
    ///
    /// Здесь же, а не отдельным вызовом: сервер меняет всё одной транзакцией,
    /// а каждый лишний вызов стоит около 230 мс. Два вызова вместо одного — это
    /// ещё и две транзакции, между которыми тикет виден наполовину изменённым.
    pub tag_edits: Option<Vec<String>>,
    /// Правки зависимостей, каждая со знаком: ["+proj-a1b2c3","-proj-d4e5f6"].
    ///
    /// Плюс — тикет НАЧИНАЕТ ждать названный; минус — перестаёт. Знак
    /// обязателен по той же причине, что у тегов: голый список однажды
    /// означал бы «заменить все», и связи пропадали бы молча. Цель обязана
    /// существовать: ребро в никуда превращает «жду такой-то тикет» в
    /// «ничего не жду», и об этом никто не узнаёт.
    pub dep_edits: Option<Vec<String>>,
    /// ЗАМЕНИТЬ весь набор зависимостей перечисленным. Пустой список снимает
    /// все. Отдельно от dep_edits: там знак обязателен, здесь его быть не
    /// должно — два разных намерения в одном поле однажды стоили нам шести
    /// потерянных тегов.
    pub dep_set: Option<Vec<String>>,
    /// Приоритет. Изменить его было нельзя вообще — потеряно при переписывании.
    pub priority: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub project: Option<String>,
    /// Срок, YYYY-MM-DD. Пустая строка снимает его.
    pub due: Option<String>,
    /// Модуль. Пустая строка снимает его. При смене проекта модуль нового
    /// проекта обязателен: молча снять его нельзя.
    pub module: Option<String>,
    /// Снять гард «тикет уже подобран». Ставить осознанно.
    pub force: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WalkArgs {
    /// Отбор по модулю — единице работы внутри проекта.
    pub module: Option<String>,
    pub workspace: String,
    /// Идентификатор сеанса обхода. Придумайте ОДИН раз и передавайте тот же на
    /// каждом шаге — по нему сервер помнит, что уже показано. Разные сеансы
    /// ходят независимо, поэтому два агента не мешают друг другу.
    pub walk_id: String,
    pub status: Option<String>,
    pub tag: Option<String>,
    pub strict: Option<bool>,
    pub title: Option<String>,
    pub assignee: Option<String>,
    pub project: Option<String>,
    pub all: Option<bool>,
    /// Забыть показанное и пойти сначала.
    pub reset: Option<bool>,
}

fn oops(e: impl std::fmt::Display) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

#[tool_router]
impl Ntk {
    pub fn new() -> Self {
        Self { tool_router: Self::tool_router() }
    }

    async fn client() -> Result<(api::Client, String), McpError> {
        let cfg = config::load().map_err(oops)?;
        let key = config::require_key(&cfg).map_err(oops)?.to_string();
        Ok((api::Client::new(&cfg.url), key))
    }

    #[tool(description = "Кто я и какие воркспейсы доступны. Зовите ПЕРВЫМ, если \
                          не знаете, какой workspace подставлять: у остальных \
                          инструментов он обязателен и значения по умолчанию нет.")]
    async fn ntk_whoami(&self) -> Result<CallToolResult, McpError> {
        let (c, key) = Self::client().await?;
        let (user, ws) = c.me(&key).await.map_err(oops)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&serde_json::json!({"user_id": user, "workspaces": ws}))
                .map_err(oops)?,
        )]))
    }

    #[tool(description = "Список тикетов воркспейса. По умолчанию только свои; \
                          all=true показывает все. Ответ постранично: limit и offset.")]
    async fn ntk_ls(&self, Parameters(a): Parameters<LsArgs>) -> Result<CallToolResult, McpError> {
        let (c, key) = Self::client().await?;
        let f = api::Filters {
            status: a.status,
            tag: a.tag,
            title: a.title,
            assignee: a.assignee,
            project: a.project,
            module: a.module.clone(),
            strict: a.strict.unwrap_or(false),
            all: a.all.unwrap_or(false),
        };
        if a.count.unwrap_or(false) {
            let n = c.count(&key, &a.workspace, &f).await.map_err(oops)?;
            return Ok(CallToolResult::success(vec![Content::text(
                serde_json::json!({"count": n}).to_string(),
            )]));
        }
        let t = c
            .tickets(&key, &a.workspace, &f, a.limit.unwrap_or(50), a.offset.unwrap_or(0))
            .await
            .map_err(oops)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&t).map_err(oops)?,
        )]))
    }

    #[tool(description = "Пройти тикеты по одному для проверки: показывает следующий \
                          ещё не показанный под этим отбором и НИЧЕГО не меняет. \
                          Не путать с ntk_next — тот берёт тикет в работу. \
                          walk_id придумайте один раз и передавайте тот же на каждом шаге.")]
    async fn ntk_walk(&self, Parameters(a): Parameters<WalkArgs>) -> Result<CallToolResult, McpError> {
        let (c, key) = Self::client().await?;
        let f = api::Filters {
            status: a.status,
            tag: a.tag,
            title: a.title,
            assignee: a.assignee,
            project: a.project,
            module: a.module.clone(),
            strict: a.strict.unwrap_or(false),
            all: a.all.unwrap_or(false),
        };
        let v = c
            .walk(&key, &a.workspace, &a.walk_id, &f, a.reset.unwrap_or(false))
            .await
            .map_err(oops)?;
        Ok(CallToolResult::success(vec![Content::text(v.to_string())]))
    }

    #[tool(description = "Тикет целиком: поля, тело, зависимости.")]
    async fn ntk_show(&self, Parameters(a): Parameters<ShowArgs>) -> Result<CallToolResult, McpError> {
        let (c, key) = Self::client().await?;
        let t = c.ticket(&key, &a.workspace, &a.id).await.map_err(oops)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&t).map_err(oops)?,
        )]))
    }

    #[tool(description = "Взять следующий свободный тикет в работу. Захват атомарный: \
                          один тикет не достанется двоим.")]
    async fn ntk_next(&self, Parameters(a): Parameters<NextArgs>) -> Result<CallToolResult, McpError> {
        let (c, key) = Self::client().await?;
        let pick = crate::api::Pick {
            tag: a.tag.as_deref(),
            strict: a.strict.unwrap_or(false),
            project: a.project.as_deref(),
            module: a.module.as_deref(),
            has_module: a.has_module.unwrap_or(false),
            assignee: a.assignee.as_deref(),
        };
        match c.next(&key, &a.workspace, a.prefer.as_deref(), &pick).await.map_err(oops)? {
            Some(t) => Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string(&t).map_err(oops)?,
            )])),
            None => Ok(CallToolResult::success(vec![Content::text(
                "свободных тикетов нет",
            )])),
        }
    }

    #[tool(description = "Завести тикет. Идентификатор придумывается клиентом \
                          и проверяется на уникальность базой.")]
    async fn ntk_create(&self, Parameters(a): Parameters<CreateArgs>) -> Result<CallToolResult, McpError> {
        let (c, key) = Self::client().await?;
        // Идентификатор назначает сервер. Здесь его не было вовсе, а сервер
        // тогда требовал его от клиента: приходил отказ разбора, и наружу это
        // выглядело как «ответ сервиса не разобрался». То есть ntk_create по
        // MCP не работал ни разу.
        let mut body = serde_json::json!({
            // Воркспейс именно в теле: ручка создания читает его оттуда, а не
            // из параметра запроса. Здесь его не было, и заведение падало с
            // «укажите workspace» — при том что воркспейс был передан.
            "workspace": a.workspace,
            "project": a.project,
            "title": a.title,
        });
        if let Some(v) = a.body { body["body"] = v.into(); }
        if let Some(v) = a.assignee { body["assignee"] = v.into(); }
        if let Some(v) = a.tags { body["tags"] = v.into(); }
        if let Some(v) = a.module { body["module"] = v.into(); }
        if let Some(v) = a.priority { body["priority"] = v.into(); }
        if let Some(v) = a.status { body["status"] = v.into(); }
        if let Some(v) = a.kind { body["type"] = v.into(); }
        if let Some(v) = a.deps { body["deps"] = v.into(); }

        match c.create(&key, &a.workspace, &body).await.map_err(oops)? {
            Some(id) => Ok(CallToolResult::success(vec![Content::text(id)])),
            None => Err(oops("сервер не смог подобрать свободный идентификатор")),
        }
    }

    #[tool(description = "Изменить тикет ОДНИМ вызовом: статус, заголовок, тело, \
                          исполнитель и теги сразу. Меняется одной транзакцией — \
                          тикет не бывает виден наполовину изменённым. Тикет вне \
                          группы todo уже кем-то подобран и требует force.")]
    async fn ntk_update(&self, Parameters(a): Parameters<UpdateArgs>) -> Result<CallToolResult, McpError> {
        let (c, key) = Self::client().await?;
        let mut body = serde_json::json!({"workspace": a.workspace});
        if let Some(v) = a.status { body["status"] = v.into(); }
        if let Some(v) = a.title { body["title"] = v.into(); }
        if a.body.is_some() && a.body_append.is_some() {
            return Err(oops("body и body_append вместе не принимаются: либо заменить тело, либо дописать"));
        }
        if let Some(v) = a.body { body["body"] = v.into(); }
        if let Some(v) = a.body_append { body["body_append"] = v.into(); }
        if let Some(v) = a.assignee { body["assignee"] = v.into(); }
        if let Some(v) = a.module { body["module"] = v.into(); }
        if let Some(t) = a.tag_edits {
            for e in &t {
                if !e.starts_with('+') && !e.starts_with('-') {
                    return Err(oops(format!("тег «{e}» без знака: нужен + или -")));
                }
            }
            body["tag_edits"] = t.into();
        }
        if let Some(d) = a.dep_edits {
            for e in &d {
                if !e.starts_with('+') && !e.starts_with('-') {
                    return Err(oops(format!(
                        "зависимость «{e}» без знака: нужен + (начать ждать) или - (перестать)"
                    )));
                }
            }
            body["dep_edits"] = d.into();
        }
        if let Some(d) = a.dep_set {
            if d.iter().any(|e| e.starts_with('+') || e.starts_with('-')) {
                return Err(oops("dep_set заменяет набор целиком — знаки здесь не нужны; для правки есть dep_edits"));
            }
            body["dep_set"] = d.into();
        }
        if let Some(v) = a.priority { body["priority"] = v.into(); }
        if let Some(v) = a.kind { body["type"] = v.into(); }
        if let Some(v) = a.project { body["project"] = v.into(); }
        if let Some(v) = a.due { body["due"] = v.into(); }
        if a.force.unwrap_or(false) { body["force"] = true.into(); }
        c.patch(&key, &a.workspace, &a.id, &body).await.map_err(oops)?;
        Ok(CallToolResult::success(vec![Content::text(format!("{} изменён", a.id))]))
    }

    #[tool(description = "Взять КОНКРЕТНЫЙ тикет в работу. Если его уже взяли, \
                          вернётся отказ с текущим статусом, а не тишина.")]
    async fn ntk_start(&self, Parameters(a): Parameters<IdArgs>) -> Result<CallToolResult, McpError> {
        let (c, key) = Self::client().await?;
        let t = c.start(&key, &a.workspace, &a.id).await.map_err(oops)?;
        Ok(CallToolResult::success(vec![Content::text(
            serde_json::to_string(&serde_json::json!({"id": t.id, "status": t.status, "title": t.title}))
                .map_err(oops)?,
        )]))
    }

    #[tool(description = "Закрыть тикет — перевести в done. Дата закрытия ставится \
                          переходом, вручную её задать нельзя.")]
    async fn ntk_close(&self, Parameters(a): Parameters<CloseArgs>) -> Result<CallToolResult, McpError> {
        let (c, key) = Self::client().await?;
        let mut body = serde_json::json!({"workspace": a.workspace, "status": "done"});
        if a.force.unwrap_or(false) {
            body["force"] = true.into();
        }
        c.patch(&key, &a.workspace, &a.id, &body).await.map_err(oops)?;
        Ok(CallToolResult::success(vec![Content::text(format!("{} закрыт", a.id))]))
    }

    #[tool(description = "Зависимости тикета: на чём он стоит и что стоит на нём.")]
    async fn ntk_deps(&self, Parameters(a): Parameters<IdArgs>) -> Result<CallToolResult, McpError> {
        let (c, key) = Self::client().await?;
        let v = c.deps(&key, &a.workspace, &a.id).await.map_err(oops)?;
        Ok(CallToolResult::success(vec![Content::text(serde_json::to_string(&v).map_err(oops)?)]))
    }

    #[tool(description = "Убрать тикет: он перестаёт показываться, но не стирается. \
                          Настоящее удаление тихо освободило бы тех, кто его ждал.")]
    async fn ntk_rm(&self, Parameters(a): Parameters<IdArgs>) -> Result<CallToolResult, McpError> {
        let (c, key) = Self::client().await?;
        let blocked = c.remove(&key, &a.workspace, &a.id).await.map_err(oops)?;
        let msg = if blocked.is_empty() {
            format!("{} убран", a.id)
        } else {
            format!("{} убран; на нём стояли: {}", a.id, blocked.join(", "))
        };
        Ok(CallToolResult::success(vec![Content::text(msg)]))
    }

    #[tool(description = "Что есть в воркспейсе: статусы и их группы, приоритеты, \
                          проекты, люди. Заменяет schema, users, projects, workspaces.")]
    async fn ntk_meta(&self, Parameters(a): Parameters<WsArgs>) -> Result<CallToolResult, McpError> {
        let (c, key) = Self::client().await?;
        let v = c.meta(&key, &a.workspace).await.map_err(oops)?;
        Ok(CallToolResult::success(vec![Content::text(serde_json::to_string(&v).map_err(oops)?)]))
    }

    #[tool(description = "Модули проекта. Показывает и действующие, и архивные: \
                          архивный виден, но выбрать его для новой работы нельзя.")]
    async fn ntk_modules(&self, Parameters(a): Parameters<ModulesArgs>) -> Result<CallToolResult, McpError> {
        let (c, key) = Self::client().await?;
        let v = c.meta(&key, &a.workspace).await.map_err(oops)?;
        let all = v.get("modules").and_then(|m| m.as_array()).cloned().unwrap_or_default();
        let rows: Vec<&serde_json::Value> = all
            .iter()
            .filter(|x| match a.project.as_deref() {
                None => true,
                Some(p) => x.get("project").and_then(|v| v.as_str()) == Some(p),
            })
            .collect();
        Ok(CallToolResult::success(vec![Content::text(serde_json::to_string(&rows).map_err(oops)?)]))
    }

    #[tool(description = "Завести модули проекта, НЕ трогая остальной реестр: ничего не уходит из действующих. Берите это вместо замены, когда нужно просто добавить имена.")]
    async fn ntk_modules_add(&self, Parameters(a): Parameters<ModulesAddArgs>) -> Result<CallToolResult, McpError> {
        if a.add.is_empty() {
            return Err(oops("назовите хотя бы один модуль"));
        }
        let (c, key) = Self::client().await?;
        let v = c.add_modules(&key, &a.workspace, &a.project, &a.add).await.map_err(oops)?;
        Ok(CallToolResult::success(vec![Content::text(serde_json::to_string(&v).map_err(oops)?)]))
    }

    #[tool(description = "Заменить список модулей проекта целиком. Список считается \
                          ПОЛНЫМ: модуль, которого в нём нет, уходит из действующих — \
                          в архив, если на него ссылаются тикеты, и насовсем, если нет. \
                          Вернувшийся в список архивный снова становится действующим. \
                          Чтобы убрать один модуль, пришлите остальные, а не его одного.")]
    async fn ntk_modules_replace(&self, Parameters(a): Parameters<ModulesReplaceArgs>) -> Result<CallToolResult, McpError> {
        if a.modules.is_empty() {
            // Пустой список стёр бы реестр проекта. Через MCP это стоит одного
            // недостающего аргумента, поэтому отказ, а не исполнение.
            return Err(oops("пустой список стёр бы весь реестр проекта: пришлите хотя бы один модуль"));
        }
        let (c, key) = Self::client().await?;
        let v = c.replace_modules(&key, &a.workspace, &a.project, &a.modules).await.map_err(oops)?;
        Ok(CallToolResult::success(vec![Content::text(serde_json::to_string(&v).map_err(oops)?)]))
    }

    #[tool(description = "Только теги, ничего больше. Если меняете что-то ещё — \
                          используйте ntk_update, он делает всё за один вызов. \
                          Каждый тег со знаком: +добавить или -убрать.")]
    async fn ntk_tag(&self, Parameters(a): Parameters<TagArgs>) -> Result<CallToolResult, McpError> {
        for t in &a.edits {
            if !t.starts_with('+') && !t.starts_with('-') {
                return Err(oops(format!("тег «{t}» без знака: нужен + или -")));
            }
        }
        let (c, key) = Self::client().await?;
        let mut body = serde_json::json!({ "workspace": a.workspace, "tag_edits": a.edits });
        if a.force.unwrap_or(false) {
            body["force"] = true.into();
        }
        c.patch(&key, &a.workspace, &a.id, &body).await.map_err(oops)?;
        Ok(CallToolResult::success(vec![Content::text(format!("теги {} изменены", a.id))]))
    }

}

#[tool_handler]
impl ServerHandler for Ntk {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            // Представляемся своим именем: по умолчанию сервер называет себя
            // именем библиотеки, и в списке клиента это выглядит как «rmcp».
            server_info: Implementation {
                name: "ntk".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                ..Default::default()
            },
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            // Инструкция уходит здесь, при подключении, а не отдельной
            // командой.
            //
            // Была команда `ntk agents`, печатавшая этот же файл, и строка
            // «выполните ntk agents» в AGENTS.md. Получалось, что агент, у
            // которого MCP уже открыт, обязан выйти в шелл за текстом, который
            // тот же самый бинарь мог отдать даром. Хуже того, печатался текст
            // УСТАНОВЛЕННОГО бинаря: пока клиент не обновлён, инструкция врёт
            // про инструмент, с которым агент реально разговаривает. Механизм
            // против расхождения его же и обеспечивал.
            //
            // Файл остаётся один на всех: он же лежит в docs/ и им же
            // заполняется раздел в AGENTS.md, если кому-то нужен текст глазами.
            instructions: Some(
                format!(
                    "Тикеты команды. Воркспейс называется явно в каждом вызове — \
                     значения по умолчанию нет, чтобы работа не ушла не туда.\n\n{}",
                    include_str!("../../../docs/agents-section.md"),
                ),
            ),
            ..Default::default()
        }
    }
}
