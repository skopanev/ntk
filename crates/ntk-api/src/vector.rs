//! Слив долга индексации: embedding у провайдера, точки в Qdrant.
//!
//! Истина — SQL. Qdrant здесь КЕШ: его потеря не теряет данных и лечится
//! повторным сливом, а его содержимое никогда не считается доказательством.
//!
//! Ни отдельного процесса, ни таймера. Слив — ограниченная задача внутри того
//! же сервиса: просыпается на старте, после записи и по уведомлению из базы.
//! Обращения к провайдеру ограничены глобальным потолком, потому что платит за
//! них владелец, а не процесс.

use anyhow::{bail, Context, Result};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, Semaphore};

/// Потолок обращений к провайдеру: 5 в секунду, задан владельцем.
///
/// Держим интервалом между отправками, а не счётчиком в окне: интервал не даёт
/// всплеска в начале секунды, а всплеск на одном ядре рядом с Postgres хуже
/// ровной нагрузки.
const MIN_GAP: Duration = Duration::from_millis(200);

/// Сколько тикетов обрабатываем одновременно.
///
/// Четыре, а не больше: задержка embedding у провайдера от трети секунды до
/// восемнадцати, и без параллельности тридцать тикетов заняли бы девять минут.
/// Но ядро одно, поэтому больше четырёх только отнимало бы его у базы, не
/// ускоряя: потолок всё равно упирается в MIN_GAP.
const CONCURRENCY: usize = 4;

/// Размерность Qwen3-Embedding-8B. Проверена живым вызовом, не взята из
/// документации.
const DIMS: u64 = 4096;

const MODEL: &str = "Qwen/Qwen3-Embedding-8B";
const COLLECTION: &str = "ntk";

pub struct Vector {
    http: reqwest::Client,
    key: String,
    qdrant: String,
    /// Время последней отправки провайдеру — общее на процесс. Именно поэтому
    /// потолок глобальный: три включённых воркспейса не дают пятнадцать в
    /// секунду вместо пяти.
    last_send: Arc<Mutex<Instant>>,
    slots: Arc<Semaphore>,
}

impl Vector {
    /// Собирается только если ключ есть. Без ключа векторизации нет, и делать
    /// вид, что она есть, нельзя: пусть отсутствие будет видно сразу.
    pub fn from_env() -> Option<Self> {
        let key = std::env::var("DEEPINFRA_API_KEY").ok().filter(|k| !k.trim().is_empty())?;
        Some(Self {
            http: reqwest::Client::builder()
                // Провайдер отвечает до восемнадцати секунд; ждать дольше
                // бессмысленно, долг всё равно сохранится и повторится.
                .timeout(Duration::from_secs(30))
                .build()
                .ok()?,
            key,
            qdrant: std::env::var("QDRANT_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:6333".into()),
            last_send: Arc::new(Mutex::new(Instant::now() - MIN_GAP)),
            slots: Arc::new(Semaphore::new(CONCURRENCY)),
        })
    }

    /// Ждёт своей очереди к провайдеру. Глобально на процесс.
    async fn pace(&self) {
        let mut last = self.last_send.lock().await;
        let since = last.elapsed();
        if since < MIN_GAP {
            tokio::time::sleep(MIN_GAP - since).await;
        }
        *last = Instant::now();
    }

    /// Коллекция создаётся при первом обращении и не пересоздаётся.
    ///
    /// Имя коллекции НЕ доказывает её непрерывности: если её потеряли и
    /// создали заново, SQL продолжит считать тикеты проиндексированными. Это
    /// лечится сверкой, а не проверкой существования, поэтому здесь только
    /// создание отсутствующей.
    pub async fn ensure_collection(&self) -> Result<()> {
        let url = format!("{}/collections/{COLLECTION}", self.qdrant);
        let r = self.http.get(&url).send().await.context("qdrant недоступен")?;
        if r.status().is_success() {
            return Ok(());
        }
        let body = serde_json::json!({
            "vectors": { "size": DIMS, "distance": "Cosine", "on_disk": true },
            // Полезная нагрузка на диске: на коробке 2 ГБ, и держать её в
            // памяти незачем — по ней только фильтруют, а не считают.
            "on_disk_payload": true
        });
        let r = self.http.put(&url).json(&body).send().await.context("qdrant недоступен")?;
        if !r.status().is_success() {
            bail!("коллекция не создалась: {}", r.text().await.unwrap_or_default());
        }
        tracing::info!(collection = COLLECTION, dims = DIMS, "коллекция создана");
        Ok(())
    }

    /// Вектор для текста. Вход обязан быть УЖЕ обрезанным: обрезка — свойство
    /// отправки и делается до подсчёта отпечатка, иначе отпечаток не совпадёт
    /// с тем, что ушло.
    async fn embed(&self, input: &str) -> Result<Vec<f32>> {
        self.pace().await;
        let r = self
            .http
            .post("https://api.deepinfra.com/v1/openai/embeddings")
            .bearer_auth(&self.key)
            .json(&serde_json::json!({ "model": MODEL, "input": input }))
            .send()
            .await
            .context("провайдер недоступен")?;
        let code = r.status();
        let v: serde_json::Value = r.json().await.context("ответ провайдера не разобрался")?;
        if !code.is_success() {
            bail!("провайдер отказал: {code} {}", v.get("error").map(|e| e.to_string()).unwrap_or_default());
        }
        let arr = v["data"][0]["embedding"]
            .as_array()
            .context("в ответе нет вектора")?
            .iter()
            .map(|x| x.as_f64().unwrap_or(0.0) as f32)
            .collect::<Vec<f32>>();
        if arr.len() as u64 != DIMS {
            bail!("размерность {} вместо {DIMS}", arr.len());
        }
        Ok(arr)
    }

    /// Точка опознаётся uuid ТИКЕТА — он уже есть, уникален и не меняется при
    /// правке текста. Хешировать для этого нечего.
    ///
    /// В нагрузке лежат воркспейс, проект, отпечаток входа и идентичность
    /// модели: по первым двум обязательный фильтр области, по остальным видно,
    /// что именно в этой точке лежит, без обращения к SQL.
    async fn upsert(
        &self,
        uuid: &str,
        vector: Vec<f32>,
        ws: &str,
        project: Option<&str>,
        ticket_id: &str,
        input_sha: &str,
    ) -> Result<()> {
        let body = serde_json::json!({
            "points": [{
                "id": uuid,
                "vector": vector,
                "payload": {
                    "workspace": ws,
                    "project": project,
                    "ticket_id": ticket_id,
                    "input_sha": input_sha,
                    "model": MODEL,
                    "input_max": crate::write::EMBED_INPUT_MAX,
                }
            }]
        });
        let r = self
            .http
            .put(format!("{}/collections/{COLLECTION}/points?wait=true", self.qdrant))
            .json(&body)
            .send()
            .await
            .context("qdrant недоступен")?;
        if !r.status().is_success() {
            bail!("точка не записалась: {}", r.text().await.unwrap_or_default());
        }
        Ok(())
    }

    async fn delete_point(&self, uuid: &str) -> Result<()> {
        let r = self
            .http
            .post(format!("{}/collections/{COLLECTION}/points/delete?wait=true", self.qdrant))
            .json(&serde_json::json!({ "points": [uuid] }))
            .send()
            .await
            .context("qdrant недоступен")?;
        if !r.status().is_success() {
            bail!("точка не удалилась: {}", r.text().await.unwrap_or_default());
        }
        Ok(())
    }
}

/// Один долг: посчитать, записать, ПРОВЕРИТЬ и только потом закрыть.
///
/// Проверка после записи — не перестраховка. Между тем, как мы прочитали текст,
/// и тем, как точка легла в Qdrant, тикет могли изменить или удалить: обращение
/// к провайдеру занимает до восемнадцати секунд. Без сверки устаревший вектор
/// пережил бы удаление и воскресил бы тикет в поиске.
///
/// Гарантия честная: не «никогда не ошибается», а «расхождение не переживает
/// следующий проход». Отпечаток разошёлся — долг остаётся, и следующий проход
/// его доделает.
async fn one(
    v: &Vector,
    pool: &deadpool_postgres::Pool,
    ws: &str,
    ticket_id: &str,
    op: &str,
    debt_sha: Option<String>,
    debt_uuid: Option<String>,
) -> Result<()> {
    let mut client = pool.get().await.context("база недоступна")?;

    // Читаем состояние тикета под ролью воркспейса.
    let (uuid, project, title, body, deleted) = {
        let tx = crate::db::begin(&mut client, ws).await?;
        let row = tx
            .query_opt(
                "select uuid::text, project_id, title, body, deleted_at is not null
                   from tickets where id = $1",
                &[&ticket_id],
            )
            .await?;
        tx.commit().await.ok();
        match row {
            Some(r) => (
                r.get::<_, String>(0),
                r.get::<_, Option<String>>(1),
                r.get::<_, String>(2),
                r.get::<_, String>(3),
                r.get::<_, bool>(4),
            ),
            // Тикета нет вовсе. Раньше здесь долг просто снимался, и точка
            // оставалась в Qdrant навсегда: uuid жил только в удалённой строке.
            // Теперь его несёт сам долг, поэтому призрака можно снести.
            None => {
                if let Some(u) = &debt_uuid {
                    v.delete_point(u).await?;
                }
                clear_debt(pool, ws, ticket_id).await?;
                return Ok(());
            }
        }
    };

    // Удаление — и когда так сказано в долге, и когда тикет уже помечен
    // удалённым. Второе важнее первого: пока долг лежал, тикет могли убрать.
    if op == "delete" || deleted {
        v.delete_point(&uuid).await?;
        let mut c = pool.get().await?;
        let tx = crate::db::begin(&mut c, ws).await?;
        tx.execute("delete from vector_index_state where ticket_id = $1", &[&ticket_id]).await?;
        tx.execute("delete from vector_debt where ticket_id = $1", &[&ticket_id]).await?;
        tx.commit().await?;
        return Ok(());
    }

    let input = crate::write::embed_input(&title, &body);
    let sha = crate::write::embed_sha(&input);
    if debt_sha.as_deref().is_some_and(|d| d != sha) {
        // Долг устарел ещё до отправки: текст успели поменять. Не платим за
        // старую версию — обновляем долг и уходим, следующий проход возьмёт
        // свежую.
        let mut c = pool.get().await?;
        let tx = crate::db::begin(&mut c, ws).await?;
        tx.execute(
            "update vector_debt set input_sha = $2, queued_at = now() where ticket_id = $1",
            &[&ticket_id, &sha],
        )
        .await?;
        tx.commit().await?;
        return Ok(());
    }

    let vector = v.embed(&input).await?;
    v.upsert(&uuid, vector, ws, project.as_deref(), ticket_id, &sha).await?;

    // Сверка ПОСЛЕ записи: тикет всё ещё жив и текст всё ещё тот?
    let mut c = pool.get().await?;
    let tx = crate::db::begin(&mut c, ws).await?;
    let now = tx
        .query_opt(
            "select title, body, deleted_at is not null from tickets where id = $1",
            &[&ticket_id],
        )
        .await?;
    match now {
        Some(r) if !r.get::<_, bool>(2) => {
            let cur = crate::write::embed_sha(&crate::write::embed_input(
                &r.get::<_, String>(0),
                &r.get::<_, String>(1),
            ));
            if cur == sha {
                tx.execute(
                    "insert into vector_index_state (ticket_id, indexed_sha, indexed_at)
                     values ($1, $2, now())
                     on conflict (ticket_id) do update
                        set indexed_sha = excluded.indexed_sha, indexed_at = now()",
                    &[&ticket_id, &sha],
                )
                .await?;
                tx.execute("delete from vector_debt where ticket_id = $1", &[&ticket_id]).await?;
            } else {
                // Разошлось за время отправки: долг остаётся со свежим
                // отпечатком, точка пока устаревшая — но выдача сверяется с SQL
                // и такую не отдаст.
                tx.execute(
                    "update vector_debt set input_sha = $2, queued_at = now() where ticket_id = $1",
                    &[&ticket_id, &cur],
                )
                .await?;
            }
            tx.commit().await?;
        }
        // Удалили пока мы считали: точку сносим немедленно, не дожидаясь
        // следующего прохода.
        _ => {
            tx.commit().await.ok();
            v.delete_point(&uuid).await?;
            let mut c2 = pool.get().await?;
            let tx2 = crate::db::begin(&mut c2, ws).await?;
            tx2.execute("delete from vector_index_state where ticket_id = $1", &[&ticket_id]).await?;
            tx2.execute("delete from vector_debt where ticket_id = $1", &[&ticket_id]).await?;
            tx2.commit().await?;
        }
    }
    Ok(())
}

async fn clear_debt(pool: &deadpool_postgres::Pool, ws: &str, id: &str) -> Result<()> {
    let mut c = pool.get().await?;
    let tx = crate::db::begin(&mut c, ws).await?;
    tx.execute("delete from vector_debt where ticket_id = $1", &[&id]).await?;
    tx.commit().await?;
    Ok(())
}

/// Обходит ТОЛЬКО те воркспейсы, где векторизация включена.
///
/// Выключенный воркспейс не сканируется вовсе — не «сканируется и ничего не
/// находит». Это разные вещи: первое даёт ноль обращений к базе воркспейса,
/// второе оставляет путь, по которому однажды что-нибудь просочится.
pub async fn enabled_workspaces(pool: &deadpool_postgres::Pool) -> Result<Vec<String>> {
    let client = pool.get().await.context("база недоступна")?;
    let rows = client
        .query("select name from core.workspaces order by name", &[])
        .await?;
    let mut out = Vec::new();
    for r in rows {
        let ws: String = r.get(0);
        let mut c = pool.get().await?;
        let Ok(tx) = crate::db::begin(&mut c, &ws).await else { continue };
        // Ошибку чтения политики не глотаем и здесь: воркспейс без миграций
        // не «выключен», он неизвестен, и молча пропустить его нельзя.
        match crate::write::vector_enabled(&tx).await {
            Ok(true) => out.push(ws.clone()),
            Ok(false) => {}
            Err(e) => tracing::warn!(error = %e, workspace = %ws, "политика векторизации не прочиталась"),
        }
        tx.commit().await.ok();
    }
    Ok(out)
}

/// Слив до пустоты. Ограниченная задача, а не бесконечный цикл: кончился долг —
/// вернулись. Следующее пробуждение придёт от записи, старта или уведомления.
pub async fn drain(v: Arc<Vector>, pool: deadpool_postgres::Pool) {
    let workspaces = match enabled_workspaces(&pool).await {
        Ok(w) => w,
        Err(e) => {
            tracing::error!(error = %e, "не удалось перечислить воркспейсы");
            return;
        }
    };
    if workspaces.is_empty() {
        return;
    }
    if let Err(e) = v.ensure_collection().await {
        tracing::error!(error = %e, "коллекция недоступна — долг остаётся");
        return;
    }

    for ws in workspaces {
        let mut done = 0usize;
        let mut failed = 0usize;
        loop {
            let batch = match take_batch(&pool, &ws).await {
                Ok(b) => b,
                Err(e) => {
                    tracing::error!(error = %e, workspace = %ws, "долг не прочитался");
                    break;
                }
            };
            if batch.is_empty() {
                break;
            }
            let mut tasks = Vec::new();
            for (id, op, sha, uuid) in batch {
                let v = v.clone();
                let pool = pool.clone();
                let ws = ws.clone();
                let slots = v.slots.clone();
                tasks.push(tokio::spawn(async move {
                    let _slot = slots.acquire().await;
                    let r = one(&v, &pool, &ws, &id, &op, sha, uuid).await;
                    if let Err(e) = &r {
                        // Отказ провайдера не откатывает SQL и не теряет долг:
                        // считаем попытку и оставляем на следующий проход.
                        tracing::warn!(error = %e, ticket = %id, workspace = %ws, "долг не слился");
                        let _ = bump_attempt(&pool, &ws, &id, &e.to_string()).await;
                    }
                    r.is_ok()
                }));
            }
            for t in tasks {
                match t.await {
                    Ok(true) => done += 1,
                    _ => failed += 1,
                }
            }
            // Все в пачке провалились — дальше долбить бессмысленно: провайдер
            // или Qdrant лежат, и следующая пачка ляжет так же.
            if done == 0 && failed > 0 {
                break;
            }
        }
        if done > 0 || failed > 0 {
            tracing::info!(workspace = %ws, done, failed, "слив долга завершён");
        }
    }
}

/// Берём пачку, пропуская то, что уже слишком часто падало.
///
/// Пять попыток — и долг остаётся лежать, но перестаёт мешать остальным: без
/// этого один битый тикет крутился бы вечно, а очередь за ним стояла.
async fn take_batch(
    pool: &deadpool_postgres::Pool,
    ws: &str,
) -> Result<Vec<(String, String, Option<String>, Option<String>)>> {
    let mut c = pool.get().await?;
    let tx = crate::db::begin(&mut c, ws).await?;
    let rows = tx
        .query(
            "select ticket_id, op, input_sha, uuid from vector_debt
              where attempts < 5 order by queued_at limit 8",
            &[],
        )
        .await?;
    tx.commit().await.ok();
    Ok(rows
        .iter()
        .map(|r| (r.get(0), r.get(1), r.get(2), r.get(3)))
        .collect())
}

async fn bump_attempt(
    pool: &deadpool_postgres::Pool,
    ws: &str,
    id: &str,
    err: &str,
) -> Result<()> {
    let mut c = pool.get().await?;
    let tx = crate::db::begin(&mut c, ws).await?;
    // Текст ошибки обрезаем: в него попадает ответ провайдера, и он бывает
    // длинным, а таблица долга не журнал.
    let short: String = err.chars().take(300).collect();
    tx.execute(
        "update vector_debt set attempts = attempts + 1, last_error = $2 where ticket_id = $1",
        &[&id, &short],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Пробуждение. Если слив уже идёт, второй не запускаем: задача одна.
pub fn wake(state: &Arc<crate::App>) {
    let Some(v) = state.vector.clone() else { return };
    if state.draining.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let pool = state.pool.clone();
    let st = state.clone();
    tokio::spawn(async move {
        drain(v, pool).await;
        st.draining.store(false, std::sync::atomic::Ordering::SeqCst);
    });
}
