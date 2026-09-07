//! Чтение из Notion: только запросы, никакой интерпретации.
//!
//! Политика повторов здесь одна на все вызовы и уважает Retry-After. Это не
//! перестраховка: потолок в три запроса в секунду делится между всеми
//! агентами сразу, и экспорт 365 тикетов с телами упирается в него гарантированно.

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const API: &str = "https://api.notion.com/v1";
const VERSION: &str = "2025-09-03";
/// Пауза между попытками, если сервер не сказал свою. Последняя цифра — это
/// уже не «подождём», а «сдаёмся, но не молча».
const BACKOFF_MS: &[u64] = &[1000, 2500, 5000, 10000];

/// Запросов в секунду. Документированный средний потолок Notion — три, и он
/// делится между всеми интеграциями сразу, поэтому держим ниже: экспорт не
/// должен мешать работающему флоту. Переопределяется NTK_RPS.
const DEFAULT_RPS: f64 = 2.0;


pub struct Notion {
    http: reqwest::Client,
    token: String,
    /// Момент, раньше которого следующий запрос отправлять нельзя.
    ///
    /// Ретраи по 429 — это лечение уже случившегося отказа: запрос ушёл,
    /// получил отлуп, ждём и повторяем. Здесь наоборот, темп держится ДО
    /// отправки, поэтому в лимит мы не влетаем вовсе. Без этого экспорт
    /// разгоняется, упирается, разгребает — и так по кругу.
    next_allowed: Mutex<Instant>,
    interval: Duration,
}

impl Notion {
    pub fn new(token: String) -> Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .context("не удалось собрать http-клиент")?,
            token,
            next_allowed: Mutex::new(Instant::now()),
            interval: Duration::from_secs_f64(1.0 / rps()),
        })
    }

    /// Пропускает не чаще, чем раз в `interval`. Держит очередь: пока один
    /// ждёт, остальные стоят за ним, а не проскакивают вперёд.
    async fn pace(&self) {
        let mut next = self.next_allowed.lock().await;
        let now = Instant::now();
        if *next > now {
            tokio::time::sleep(*next - now).await;
        }
        *next = Instant::now() + self.interval;
    }

    async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        for (attempt, wait) in BACKOFF_MS.iter().enumerate() {
            self.pace().await;
            let resp = self
                .http
                .post(format!("{API}{path}"))
                .bearer_auth(&self.token)
                .header("Notion-Version", VERSION)
                .json(body)
                .send()
                .await
                .with_context(|| format!("запрос к {path} не ушёл"))?;

            let status = resp.status();
            if status.is_success() {
                return Ok(resp.json().await.context("ответ не разобрался как JSON")?);
            }
            // 429 и 5xx — отказ ДО того, как запрос что-то сделал, поэтому
            // повтор безопасен. Всё остальное повторять нельзя: ответ уже дан.
            if !(status.as_u16() == 429 || status.is_server_error()) || attempt == BACKOFF_MS.len() - 1 {
                let text = resp.text().await.unwrap_or_default();
                bail!("{path}: {status} {text}");
            }
            let told = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .map(|s| s * 1000);
            tokio::time::sleep(Duration::from_millis(told.unwrap_or(*wait))).await;
        }
        unreachable!("цикл повторов всегда завершается возвратом или ошибкой")
    }

    async fn get(&self, path: &str) -> Result<Value> {
        for (attempt, wait) in BACKOFF_MS.iter().enumerate() {
            self.pace().await;
            let resp = self
                .http
                .get(format!("{API}{path}"))
                .bearer_auth(&self.token)
                .header("Notion-Version", VERSION)
                .send()
                .await
                .with_context(|| format!("запрос к {path} не ушёл"))?;
            let status = resp.status();
            if status.is_success() {
                return Ok(resp.json().await.context("ответ не разобрался как JSON")?);
            }
            if !(status.as_u16() == 429 || status.is_server_error()) || attempt == BACKOFF_MS.len() - 1 {
                let text = resp.text().await.unwrap_or_default();
                bail!("{path}: {status} {text}");
            }
            let told = resp.headers().get("retry-after")
                .and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok()).map(|s| s * 1000);
            tokio::time::sleep(Duration::from_millis(told.unwrap_or(*wait))).await;
        }
        unreachable!()
    }

    pub async fn data_source_id(&self, database_id: &str) -> Result<String> {
        let db = self.get(&format!("/databases/{database_id}")).await?;
        db.pointer("/data_sources/0/id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .with_context(|| format!("у базы {database_id} нет data source"))
    }

    /// Все страницы базы. Здесь полная выборка уместна: снимок и должен быть
    /// полным — в отличие от команд, которые ради одного тикета читали всё.
    pub async fn all_pages(&self, data_source_id: &str) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let mut body = json!({ "page_size": 100 });
            if let Some(c) = &cursor {
                body["start_cursor"] = json!(c);
            }
            let resp = self.post(&format!("/data_sources/{data_source_id}/query"), &body).await?;
            if let Some(results) = resp.get("results").and_then(Value::as_array) {
                out.extend(results.iter().cloned());
            }
            match resp.get("next_cursor").and_then(Value::as_str) {
                Some(c) if resp.get("has_more").and_then(Value::as_bool) == Some(true) => {
                    cursor = Some(c.to_string());
                }
                _ => break,
            }
        }
        Ok(out)
    }

    /// Дети блока, все страницы.
    pub async fn children(&self, block_id: &str) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let q = match &cursor {
                Some(c) => format!("/blocks/{block_id}/children?page_size=100&start_cursor={c}"),
                None => format!("/blocks/{block_id}/children?page_size=100"),
            };
            let resp = self.get(&q).await?;
            if let Some(results) = resp.get("results").and_then(Value::as_array) {
                out.extend(results.iter().cloned());
            }
            match resp.get("next_cursor").and_then(Value::as_str) {
                Some(c) if resp.get("has_more").and_then(Value::as_bool) == Some(true) => {
                    cursor = Some(c.to_string());
                }
                _ => break,
            }
        }
        Ok(out)
    }
}

/// Темп задаётся окружением, чтобы не пересобирать ради подбора числа.
fn rps() -> f64 {
    std::env::var("NTK_RPS")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
        .filter(|v| *v > 0.0 && *v <= 10.0)
        .unwrap_or(DEFAULT_RPS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn requests_are_spaced_apart() {
        let n = Notion::new("t".into()).unwrap();
        let started = Instant::now();
        for _ in 0..4 {
            n.pace().await;
        }
        // Четыре пропуска при трёх в секунду — не меньше двух интервалов
        // (первый уходит сразу). Проверяем нижнюю границу, а не точное время:
        // планировщик может задержать, но обогнать темп не может.
        assert!(
            started.elapsed() >= Duration::from_millis(600),
            "темп не выдержан: {:?}",
            started.elapsed()
        );
    }
}


