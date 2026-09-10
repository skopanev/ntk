//! Разговор с сервисом. Всё, что клиент знает о сервере, — здесь.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::time::Duration;

/// Отборы списка. Пустое поле — «не отбирать по этому».
#[derive(Default, Clone)]
pub struct Filters {
    pub status: Option<String>,
    pub tag: Option<String>,
    pub title: Option<String>,
    pub assignee: Option<String>,
    pub project: Option<String>,
    pub module: Option<String>,
    pub strict: bool,
    /// Тикеты всех, а не только свои. Проигрывает явно названному исполнителю.
    pub all: bool,
    /// Только те, что стоят в текущем статусе дольше N дней.
    pub stale: Option<i64>,
}

pub struct Client {
    http: reqwest::Client,
    base: String,
}

/// Что сервер сообщает о захваченном тикете: не содержимое, а факт захвата.
#[derive(Deserialize, serde::Serialize)]
pub struct Claimed {
    pub id: String,
    pub title: String,
    pub status: String,
}

#[derive(Deserialize)]
pub struct DeviceStart {
    pub code: String,
    pub device_secret: String,
    pub verification_url: String,
    pub expires_in: i64,
}

/// Ответ опроса. Поля `status` в сервере есть, но клиенту оно не нужно:
/// решение принимается по HTTP-коду и наличию ключа, а дублирующий признак
/// разошёлся бы с ними при первом же расхождении.
#[derive(Deserialize)]
struct Poll {
    key: Option<String>,
    error: Option<String>,
}

/// Отборы в строку запроса — ОДНИМ местом на все ручки.
///
/// Раньше каждая ручка собирала их сама, и `walk` потерял `module`: clap его
/// принимал, `Filters` нёс, до сервиса он не доезжал, а отбор выглядел
/// применённым — обход отдавал весь набор. Пока перечень полей повторяется
/// трижды, потеря поля в одном из них — вопрос времени, а не внимательности.
fn filter_query(f: &Filters) -> Vec<(&'static str, String)> {
    let mut q: Vec<(&'static str, String)> = Vec::new();
    if let Some(v) = f.status.as_deref() { q.push(("status", v.to_string())); }
    if let Some(v) = f.tag.as_deref() { q.push(("tag", v.to_string())); }
    if let Some(v) = f.title.as_deref() { q.push(("title", v.to_string())); }
    if let Some(v) = f.assignee.as_deref() { q.push(("assignee", v.to_string())); }
    if let Some(v) = f.project.as_deref() { q.push(("project", v.to_string())); }
    if let Some(v) = f.module.as_deref() { q.push(("module", v.to_string())); }
    if f.strict { q.push(("strict", "true".to_string())); }
    if f.all { q.push(("all", "true".to_string())); }
    if let Some(d) = f.stale { q.push(("stale", d.to_string())); }
    q
}

/// Отборы захвата. Отдельно от `prefer` намеренно: prefer задаёт ПОРЯДОК и
/// ничего не отсекает, отбор ИСКЛЮЧАЕТ.
#[derive(Default)]
pub struct Pick<'a> {
    pub tag: Option<&'a str>,
    pub strict: bool,
    pub project: Option<&'a str>,
    pub module: Option<&'a str>,
    pub has_module: bool,
    pub assignee: Option<&'a str>,
    /// Показать, что БЫ взялось, ничего не забирая.
    pub dry_run: bool,
}

impl Client {
    pub fn new(base: &str) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                // Соединение переиспользуется между вызовами.
                //
                // Для CLI это ничего не даёт: процесс живёт один запрос. Зато
                // в режиме MCP процесс живёт часами и делает десятки вызовов, а
                // TLS-рукопожатие стоит дороже самого запроса — замерено на
                // дроплете: 100 запросов напрямую к сервису 2.8 с, те же через
                // TLS 10.9 с. Восемь секунд из одиннадцати уходило на
                // установку соединений, которые можно было не устанавливать.
                .pool_idle_timeout(Duration::from_secs(90))
                .pool_max_idle_per_host(8)
                .tcp_keepalive(Duration::from_secs(60))
                .build()
                .expect("http-клиент"),
            base: base.trim_end_matches('/').to_string(),
        }
    }

    pub async fn device_start(&self) -> Result<DeviceStart> {
        self.http
            .post(format!("{}/device/start", self.base))
            .send()
            .await
            .context("the service is unavailable")?
            .json()
            .await
            .context("the service response did not parse")
    }

    /// Ждёт, пока человек пройдёт вход в браузере.
    ///
    /// Опрашиваем раз в две секунды и не чаще: сервис отвечает мгновенно, а
    /// частый опрос ничего не ускоряет — узкое место здесь человек.
    pub async fn device_wait(&self, code: &str, secret: &str, seconds: i64) -> Result<String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(seconds.max(1) as u64);
        loop {
            let r = self
                .http
                .post(format!("{}/device/poll", self.base))
                .json(&serde_json::json!({"code": code, "device_secret": secret}))
                .send()
                .await
                .context("the service is unavailable")?;
            let status = r.status();
            let body: Poll = r.json().await.unwrap_or(Poll { key: None, error: None });

            if let Some(k) = body.key {
                return Ok(k);
            }
            if status == reqwest::StatusCode::GONE {
                bail!("{}", body.error.unwrap_or_else(|| "the code has expired".into()));
            }
            if std::time::Instant::now() >= deadline {
                bail!("the sign-in did not finish in time — run ntk login again");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    /// Кто я и какие воркспейсы доступны.
    pub async fn me(&self, key: &str) -> Result<(String, Vec<String>)> {
        #[derive(Deserialize)]
        struct Me { user_id: String, #[serde(default)] workspaces: Vec<String> }
        let r = self.http.get(format!("{}/v1/me", self.base)).bearer_auth(key).send().await
            .context("the service is unavailable")?;
        if !r.status().is_success() { bail!("the key was not accepted: {}", r.status()); }
        let m: Me = r.json().await.context("the service response did not parse")?;
        Ok((m.user_id, m.workspaces))
    }

    /// Отборы списка одним значением.
    ///
    /// Сведены в структуру не для красоты: их стало семь, и позиционные
    /// аргументы начали путаться местами — два `Option<&str>` подряд
    /// компилятор молча пропускает.
    pub async fn tickets(
        &self,
        key: &str,
        workspace: &str,
        f: &Filters,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<ntk_core::Ticket>> {
        #[derive(Deserialize)]
        struct Wrap {
            #[serde(default)]
            tickets: Vec<ntk_core::Ticket>,
            #[serde(default)]
            error: Option<String>,
        }
        let mut req = self
            .http
            .get(format!("{}/v1/tickets", self.base))
            .bearer_auth(key)
            .query(&[
                ("workspace", workspace),
                ("limit", &limit.to_string()),
                ("offset", &offset.to_string()),
            ]);
        req = req.query(&filter_query(f));
        let r = req.send().await.context("the service is unavailable")?;
        let code = r.status();
        let w: Wrap = r.json().await.context("the service response did not parse")?;
        if !code.is_success() {
            bail!("{}", w.error.unwrap_or_else(|| code.to_string()));
        }
        Ok(w.tickets)
    }

    /// Один тикет со всем, что у него есть, включая зависимости.
    pub async fn ticket(&self, key: &str, workspace: &str, id: &str) -> Result<ntk_core::Ticket> {
        #[derive(Deserialize)]
        struct Wrap {
            ticket: Option<ntk_core::Ticket>,
            #[serde(default)]
            error: Option<String>,
        }
        let r = self
            .http
            .get(format!("{}/v1/tickets/{}", self.base, urlencode(id)))
            .bearer_auth(key)
            .query(&[("workspace", workspace)])
            .send()
            .await
            .context("the service is unavailable")?;
        let code = r.status();
        let w: Wrap = r.json().await.context("the service response did not parse")?;
        match w.ticket {
            Some(t) if code.is_success() => Ok(t),
            _ => bail!("{}", w.error.unwrap_or_else(|| code.to_string())),
        }
    }

    /// Захват следующего свободного тикета.
    ///
    /// `prefer` — ПОРЯДОК предпочтения тегов, а не фильтр: когда тикеты с
    /// первым тегом кончились, берётся следующий, и агент не простаивает.
    ///
    /// Сервер отвечает плоско — id, title, status, — а не целым тикетом:
    /// захват сообщает, что взято, а не показывает содержимое. Я сперва
    /// написал разбор `{"ticket": {...}}` по догадке, и клиент печатал
    /// «свободных тикетов нет», когда сервер тикет только что выдал.
    pub async fn next(
        &self,
        key: &str,
        workspace: &str,
        prefer: Option<&str>,
        f: &Pick<'_>,
    ) -> Result<Option<Claimed>> {
        let mut req = self
            .http
            .post(format!("{}/v1/tickets/next", self.base))
            .bearer_auth(key)
            .query(&[("workspace", workspace)]);
        if let Some(p) = prefer {
            req = req.query(&[("prefer", p)]);
        }
        if let Some(v) = f.tag { req = req.query(&[("tag", v)]); }
        if f.strict { req = req.query(&[("strict", "true")]); }
        if let Some(v) = f.project { req = req.query(&[("project", v)]); }
        if let Some(v) = f.module { req = req.query(&[("module", v)]); }
        if f.has_module { req = req.query(&[("has_module", "true")]); }
        if let Some(v) = f.assignee { req = req.query(&[("assignee", v)]); }
        if f.dry_run { req = req.query(&[("dry_run", "true")]); }
        let r = req.send().await.context("the service is unavailable")?;
        let code = r.status();
        // «Свободных нет» приходит как 204 с ПУСТЫМ телом, и разбирать его как
        // JSON нельзя: клиент отвечал «ответ сервиса не разобрался» вместо
        // спокойного «свободных нет». Ошибка вылезала только на пустой очереди
        // — то есть ровно тогда, когда агенту и без того нечего делать.
        if code == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        let body: serde_json::Value = r.json().await.context("the service response did not parse")?;

        if !code.is_success() {
            bail!("{}", body.get("error").and_then(|e| e.as_str()).unwrap_or(code.as_str()));
        }
        // Пусто — это ответ, а не ошибка: свободных тикетов может не быть.
        match body.get("id").and_then(|v| v.as_str()) {
            None => Ok(None),
            Some(id) => Ok(Some(Claimed {
                id: id.to_string(),
                title: body.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                status: body.get("status").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            })),
        }
    }

    /// Заведение тикета. Возвращает Ok(None), если идентификатор занят —
    /// вызывающий придумает другой хвост и повторит.
    #[allow(clippy::too_many_arguments)]
    pub async fn create(&self, key: &str, workspace: &str, body: &serde_json::Value) -> Result<Option<String>> {
        let r = self
            .http
            .post(format!("{}/v1/tickets", self.base))
            .bearer_auth(key)
            .query(&[("workspace", workspace)])
            .json(body)
            .send()
            .await
            .context("the service is unavailable")?;
        let code = r.status();
        let v: serde_json::Value = r.json().await.context("the service response did not parse")?;
        if code == reqwest::StatusCode::CONFLICT {
            // Тем же кодом отвечают две разные вещи: «не подобрался свободный
            // идентификатор» и «похоже, это уже заведено». Различаем по списку
            // похожих, иначе остановка на дубле читалась бы как исчерпание
            // идентификаторов — то есть как поломка сервиса.
            if let Some(sim) = v.get("similar").and_then(|s| s.as_array()) {
                bail!(
                    "{}\n{}",
                    v.get("error").and_then(|e| e.as_str()).unwrap_or("this looks like it has already been filed"),
                    render_similar(sim)
                );
            }
            return Ok(None);
        }
        if !code.is_success() {
            bail!("{}", v.get("error").and_then(|e| e.as_str()).unwrap_or(code.as_str()));
        }
        Ok(Some(v.get("id").and_then(|i| i.as_str()).unwrap_or_default().to_string()))
    }

    /// Похожие тикеты. Текст уходит телом: в строке запроса ему не место.
    pub async fn similar(
        &self,
        key: &str,
        workspace: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        // Воркспейс идёт В ТЕЛЕ: ручка читает его оттуда. Параметра запроса
        // здесь мало — на нём заведение уже один раз молча падало с «укажите
        // workspace» при переданном воркспейсе.
        let mut body = body.clone();
        body["workspace"] = workspace.into();
        let r = self
            .http
            .post(format!("{}/v1/find", self.base))
            .bearer_auth(key)
            .json(&body)
            .send()
            .await
            .context("the service is unavailable")?;
        let code = r.status();
        let v: serde_json::Value = r.json().await.context("the service response did not parse")?;
        if !code.is_success() {
            bail!("{}", v.get("error").and_then(|e| e.as_str()).unwrap_or(code.as_str()));
        }
        Ok(v)
    }

}

/// Список похожих человеку: оценка, идентификатор, статус, заголовок.
pub fn render_similar(hits: &[serde_json::Value]) -> String {
    hits.iter()
        .map(|h| {
            let g = |k: &str| h.get(k).and_then(|v| v.as_str()).unwrap_or("");
            format!(
                "  {:.2}  {:<22} {:<12} {}",
                h.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0),
                g("id"),
                g("status"),
                g("title")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl Client {
    /// Сколько подходящих тикетов. Считает база: листать страницами и
    /// складывать — и медленно, и неверно, если между страницами что-то
    /// изменилось.
    pub async fn count(&self, key: &str, workspace: &str, f: &Filters) -> Result<i64> {
        #[derive(Deserialize)]
        struct Wrap {
            #[serde(default)]
            count: i64,
            #[serde(default)]
            error: Option<String>,
        }
        let mut req = self
            .http
            .get(format!("{}/v1/tickets", self.base))
            .bearer_auth(key)
            .query(&[("workspace", workspace), ("count", "true")]);
        req = req.query(&filter_query(f));
        let r = req.send().await.context("the service is unavailable")?;
        let code = r.status();
        let w: Wrap = r.json().await.context("the service response did not parse")?;
        if !code.is_success() {
            bail!("{}", w.error.unwrap_or_else(|| code.to_string()));
        }
        Ok(w.count)
    }

    /// Шаг обхода: следующий непросмотренный тикет под тем же отбором.
    ///
    /// Место обхода живёт на сервере, а не здесь: ходить хотят и из терминала,
    /// и из Claude, а две памяти означали бы два разных «уже смотрел».
    pub async fn walk(
        &self,
        key: &str,
        workspace: &str,
        walk_id: &str,
        f: &Filters,
        reset: bool,
    ) -> Result<serde_json::Value> {
        let mut req = self
            .http
            .post(format!("{}/v1/tickets/walk", self.base))
            .bearer_auth(key)
            .query(&[("workspace", workspace), ("walk_id", walk_id)]);
        if reset {
            req = req.query(&[("reset", "true")]);
        }
        req = req.query(&filter_query(f));

        let r = req.send().await.context("the service is unavailable")?;
        let code = r.status();
        let v: serde_json::Value = r.json().await.context("the service response did not parse")?;
        if !code.is_success() {
            bail!("{}", v.get("error").and_then(|e| e.as_str()).unwrap_or(code.as_str()));
        }
        Ok(v)
    }

    /// Правка тикета: статус, заголовок, тело, исполнитель.
    ///
    /// `closed_at` здесь не передаётся никогда — его ставит триггер при
    /// переходе. Подставить его руками значило бы записать время, когда
    /// команда выполнилась, а не когда работа закончилась.
    pub async fn patch(&self, key: &str, workspace: &str, id: &str, body: &serde_json::Value) -> Result<()> {
        let r = self
            .http
            .patch(format!("{}/v1/tickets/{}", self.base, urlencode(id)))
            .bearer_auth(key)
            .query(&[("workspace", workspace)])
            .json(body)
            .send()
            .await
            .context("the service is unavailable")?;
        let code = r.status();
        let v: serde_json::Value = r.json().await.unwrap_or_default();
        if !code.is_success() {
            bail!("{}", v.get("error").and_then(|e| e.as_str()).unwrap_or(code.as_str()));
        }
        Ok(())
    }

    /// Захват конкретного тикета. 409 означает, что его уже взяли, и это
    /// ответ, а не сбой: сообщение сервера содержит текущий статус.
    pub async fn start(&self, key: &str, workspace: &str, id: &str) -> Result<Claimed> {
        let r = self
            .http
            .post(format!("{}/v1/tickets/{}/start", self.base, urlencode(id)))
            .bearer_auth(key)
            .query(&[("workspace", workspace)])
            .send()
            .await
            .context("the service is unavailable")?;
        let code = r.status();
        let v: serde_json::Value = r.json().await.context("the service response did not parse")?;
        if !code.is_success() {
            bail!("{}", v.get("error").and_then(|e| e.as_str()).unwrap_or(code.as_str()));
        }
        Ok(Claimed {
            id: v.get("id").and_then(|x| x.as_str()).unwrap_or(id).to_string(),
            title: v.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            status: v.get("status").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        })
    }


    /// Дерево зависимостей одного тикета: вверх и вниз.
    pub async fn deps(&self, key: &str, workspace: &str, id: &str) -> Result<serde_json::Value> {
        let r = self
            .http
            .get(format!("{}/v1/tickets/{}/deps", self.base, urlencode(id)))
            .bearer_auth(key)
            .query(&[("workspace", workspace)])
            .send()
            .await
            .context("the service is unavailable")?;
        let code = r.status();
        let v: serde_json::Value = r.json().await.context("the service response did not parse")?;
        if !code.is_success() {
            bail!("{}", v.get("error").and_then(|e| e.as_str()).unwrap_or(code.as_str()));
        }
        Ok(v)
    }


    /// Удаление тикета. Возвращает список тех, кто на нём стоял.
    pub async fn remove(&self, key: &str, workspace: &str, id: &str) -> Result<Vec<String>> {
        let r = self
            .http
            .delete(format!("{}/v1/tickets/{}", self.base, urlencode(id)))
            .bearer_auth(key)
            .query(&[("workspace", workspace)])
            .send()
            .await
            .context("the service is unavailable")?;
        let code = r.status();
        let v: serde_json::Value = r.json().await.context("the service response did not parse")?;
        if !code.is_success() {
            bail!("{}", v.get("error").and_then(|e| e.as_str()).unwrap_or(code.as_str()));
        }
        Ok(v.get("still_waiting_on_it")
            .and_then(|x| x.as_array())
            .map(|a| a.iter().filter_map(|i| i.as_str().map(str::to_string)).collect())
            .unwrap_or_default())
    }

    /// Что есть в воркспейсе: статусы, приоритеты, проекты, люди.
    pub async fn meta(&self, key: &str, workspace: &str) -> Result<serde_json::Value> {
        let r = self
            .http
            .get(format!("{}/v1/meta", self.base))
            .bearer_auth(key)
            .query(&[("workspace", workspace)])
            .send()
            .await
            .context("the service is unavailable")?;
        let code = r.status();
        let v: serde_json::Value = r.json().await.context("the service response did not parse")?;
        if !code.is_success() {
            bail!("{}", v.get("error").and_then(|e| e.as_str()).unwrap_or(code.as_str()));
        }
        Ok(v)
    }


    /// Замена списка модулей проекта целиком.
    /// Убрать проект из выбора или вернуть в него.
    pub async fn set_project_archived(
        &self, key: &str, workspace: &str, project: &str, archived: bool,
    ) -> Result<serde_json::Value> {
        let r = self
            .http
            .patch(format!("{}/v1/projects/{}", self.base, urlencode(project)))
            .bearer_auth(key)
            .json(&serde_json::json!({ "workspace": workspace, "archived": archived }))
            .send()
            .await
            .context("the service is unavailable")?;
        let code = r.status();
        let v: serde_json::Value = r.json().await.context("the service response did not parse")?;
        if !code.is_success() {
            bail!("{}", v.get("error").and_then(|e| e.as_str()).unwrap_or(code.as_str()));
        }
        Ok(v)
    }

    /// Перенести все тикеты проекта в другой — одной транзакцией на сервисе.
    pub async fn move_project(
        &self, key: &str, workspace: &str, from: &str, to: &str,
    ) -> Result<serde_json::Value> {
        let r = self
            .http
            .post(format!("{}/v1/projects/{}/move", self.base, urlencode(from)))
            .bearer_auth(key)
            .json(&serde_json::json!({ "workspace": workspace, "to": to }))
            .send()
            .await
            .context("the service is unavailable")?;
        let code = r.status();
        let v: serde_json::Value = r.json().await.context("the service response did not parse")?;
        if !code.is_success() {
            bail!("{}", v.get("error").and_then(|e| e.as_str()).unwrap_or(code.as_str()));
        }
        Ok(v)
    }

    /// Завести модули, не трогая остальной реестр.
    pub async fn add_modules(
        &self, key: &str, workspace: &str, project: &str, add: &[String],
    ) -> Result<serde_json::Value> {
        let r = self
            .http
            .post(format!("{}/v1/projects/{}/modules", self.base, urlencode(project)))
            .bearer_auth(key)
            .json(&serde_json::json!({ "workspace": workspace, "add": add }))
            .send()
            .await
            .context("the service is unavailable")?;
        let code = r.status();
        let v: serde_json::Value = r.json().await.context("the service response did not parse")?;
        if !code.is_success() {
            bail!("{}", v.get("error").and_then(|e| e.as_str()).unwrap_or(code.as_str()));
        }
        Ok(v)
    }

    pub async fn replace_modules(
        &self, key: &str, workspace: &str, project: &str, modules: &[String],
    ) -> Result<serde_json::Value> {
        let r = self
            .http
            .put(format!("{}/v1/projects/{}/modules", self.base, urlencode(project)))
            .bearer_auth(key)
            .json(&serde_json::json!({ "workspace": workspace, "modules": modules }))
            .send()
            .await
            .context("the service is unavailable")?;
        let code = r.status();
        let v: serde_json::Value = r.json().await.context("the service response did not parse")?;
        if !code.is_success() {
            bail!("{}", v.get("error").and_then(|e| e.as_str()).unwrap_or(code.as_str()));
        }
        Ok(v)
    }

}

/// Идентификаторы содержат только буквы, цифры, дефис и подчёркивание, но
/// пользователь может набрать что угодно, и это уходит в путь запроса.
fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}
#[cfg(test)]
mod filter_query_tests {
    use super::{filter_query, Filters};

    fn full() -> Filters {
        Filters {
            status: Some("open".into()),
            tag: Some("infra".into()),
            title: Some("заголовок".into()),
            assignee: Some("sk".into()),
            project: Some("ntk".into()),
            module: Some("server/db".into()),
            strict: true,
            all: true,
            stale: Some(7),
        }
    }

    // Отбор, названный в командной строке, обязан доехать до сервиса. Обход
    // терял именно module: он принимался и молча не применялся, а выдача
    // выглядела отфильтрованной. Проверяем ВСЕ поля разом — теперь строку
    // запроса собирает одно место, поэтому один тест закрывает все ручки.
    #[test]
    fn every_filter_reaches_the_query() {
        let q = filter_query(&full());
        for want in ["status", "tag", "title", "assignee", "project", "module", "strict", "all", "stale"] {
            assert!(q.iter().any(|(k, _)| *k == want), "потерян отбор {want}: {q:?}");
        }
    }

    #[test]
    fn module_carries_its_value() {
        let q = filter_query(&full());
        let m = q.iter().find(|(k, _)| *k == "module").expect("module");
        assert_eq!(m.1, "server/db");
    }

    #[test]
    fn empty_filters_add_nothing() {
        let q = filter_query(&Filters {
            status: None, tag: None, title: None, assignee: None,
            project: None, module: None, strict: false, all: false, stale: None,
        });
        assert!(q.is_empty(), "пустой отбор не должен ничего добавлять: {q:?}");
    }
}
