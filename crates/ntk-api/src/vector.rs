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

/// Отказ, в котором виноват ВХОД, а не мир.
///
/// Различие не про аккуратность отчётов, а про то, кто платит за простой.
/// Счётчик попыток гасит долг после пяти неудач — задумано против одного
/// битого тикета, который иначе крутился бы вечно. Но пока причина не
/// различалась, десятиминутный обморок провайдера жёг попытки ВСЕМ долгам
/// подряд: пять пробуждений при живом трафике — это секунды, и вся очередь
/// уходила в мёртвые, включая заведомо здоровые тикеты.
///
/// Хуже, чем потеря денег: у мёртвого долга точка в Qdrant остаётся вечно
/// старой, а по ней теперь ищут похожих. Отказ в заведении сослался бы на
/// тикет, который давно переписан в другую сторону, — и сделал бы это с
/// уверенным видом.
#[derive(Debug)]
pub struct InputFault;

impl std::fmt::Display for InputFault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "провайдер отверг сам вход")
    }
}

impl std::error::Error for InputFault {}
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
            let detail = v.get("error").map(|e| e.to_string()).unwrap_or_default();
            // Вина входа — это ФОРМА присланного, и только она. Здесь стояло
            // «любой 4xx, кроме 429», и в эту щель попадали 401 (ключ протух),
            // 402 (кончился баланс) и 403: коды клиентские, но текст в них не
            // виноват — тот же самый пройдёт, как только починят ключ или
            // оплатят счёт. А засчитанная попытка при протухшем ключе увела бы
            // в мёртвые всю очередь за пять пробуждений, то есть ровно тот
            // механизм, который здесь и чинился, пережил бы починку.
            if matches!(code.as_u16(), 400 | 413 | 422) {
                return Err(anyhow::Error::new(InputFault)
                    .context(format!("провайдер отказал: {code} {detail}")));
            }
            bail!("провайдер отказал: {code} {detail}");
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

    /// Поиск по вектору внутри одного воркспейса.
    ///
    /// Фильтр по воркспейсу обязателен и стоит в самом запросе, а не в разборе
    /// ответа: коллекция одна на все воркспейсы, и «отфильтруем потом» здесь
    /// значит «однажды не отфильтруем». Возвращаем идентификаторы тикетов и
    /// оценку, а не сами тикеты: содержимое берётся из SQL, где оно
    /// единственно верное.
    async fn search(
        &self,
        vector: Vec<f32>,
        ws: &str,
        limit: usize,
        min_score: f64,
    ) -> Result<Vec<(String, f64)>> {
        let body = serde_json::json!({
            "vector": vector,
            "limit": limit,
            "score_threshold": min_score,
            "with_payload": true,
            "filter": { "must": [{ "key": "workspace", "match": { "value": ws } }] }
        });
        let r = self
            .http
            .post(format!("{}/collections/{COLLECTION}/points/search", self.qdrant))
            .json(&body)
            .send()
            .await
            .context("qdrant недоступен")?;
        let code = r.status();
        let v: serde_json::Value = r.json().await.context("ответ qdrant не разобрался")?;
        if !code.is_success() {
            bail!("поиск не удался: {code} {v}");
        }
        Ok(v["result"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|h| {
                        Some((
                            h["payload"]["ticket_id"].as_str()?.to_string(),
                            h["score"].as_f64().unwrap_or(0.0),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Все точки воркспейса: идентификатор точки и тикета.
    ///
    /// Страницами: коллекция одна на все воркспейсы, и «возьмём сразу все»
    /// однажды упрётся в память там, где никто не смотрит.
    async fn all_points(&self, ws: &str) -> Result<Vec<Point>> {
        let mut out = Vec::new();
        let mut offset: Option<serde_json::Value> = None;
        loop {
            let mut body = serde_json::json!({
                "limit": 256,
                "with_payload": true,
                "with_vector": false,
                "filter": { "must": [{ "key": "workspace", "match": { "value": ws } }] }
            });
            if let Some(o) = &offset {
                body["offset"] = o.clone();
            }
            let r = self
                .http
                .post(format!("{}/collections/{COLLECTION}/points/scroll", self.qdrant))
                .json(&body)
                .send()
                .await
                .context("qdrant недоступен")?;
            let code = r.status();
            let v: serde_json::Value = r.json().await.context("ответ qdrant не разобрался")?;
            if !code.is_success() {
                bail!("обход точек не удался: {code} {v}");
            }
            let page = v["result"]["points"].as_array().cloned().unwrap_or_default();
            for p in &page {
                let (Some(id), Some(tid)) = (
                    p["id"].as_str().map(str::to_string),
                    p["payload"]["ticket_id"].as_str().map(str::to_string),
                ) else {
                    continue;
                };
                out.push(Point {
                    id,
                    ticket: tid,
                    sha: p["payload"]["input_sha"].as_str().map(str::to_string),
                    model: p["payload"]["model"].as_str().map(str::to_string),
                    input_max: p["payload"]["input_max"].as_u64().map(|n| n as usize),
                });
            }
            match v["result"]["next_page_offset"].clone() {
                serde_json::Value::Null => break,
                next => offset = Some(next),
            }
            if page.is_empty() {
                break;
            }
        }
        Ok(out)
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

/// Точка Qdrant глазами сверки.
///
/// Нагрузка несёт модель и предел обрезки не для красоты: отпечаток считается
/// по ТЕКСТУ, и смена модели или предела его не меняет. Точка остаётся
/// «свежей» по sha, а смысл у неё уже другой — индекс становится смешанным,
/// оценки в нём несравнимы между собой, и заметить это нечем. Поэтому сверка
/// читает и их.
struct Point {
    id: String,
    ticket: String,
    sha: Option<String>,
    model: Option<String>,
    input_max: Option<usize>,
}

/// Порог, выше которого тикет считаем возможным дублем.
///
/// Косинус, поэтому 1.0 — тот же текст. Значение ИЗМЕРЕНО, и мерить пришлось
/// дважды: первый замер я поставил не в той форме и чуть не выбрал порог по
/// чужому распределению.
///
/// Сравнений здесь два РАЗНЫХ, и путать их нельзя:
///
/// 1. Короткий текст НОВОГО тикета против тел старых. Только это и происходит
///    на заведении, и только это управляет отказом. Замер на 30 боевых
///    тикетах: четыре пересказа существующих тикетов своими словами дали
///    0.7795–0.8971, одиннадцать НЕ дублей — 0.2909–0.6357, причём верхнюю
///    границу держит самый трудный случай, какой удалось придумать: соседняя
///    работа в том же файле («закрепить версию Bun в CI» против «два гарда не
///    типизируются из-за отсутствия объявлений Bun»). Зазор 0.6357..0.7795.
/// 2. Старый тикет против старого, оба целиком. Так сравнивает только
///    ntk_similar с --id. Там верх не-дублей выше — 0.7631 на 130 парах, —
///    потому что длинное с длинным всегда ближе, чем короткое с длинным.
///    Порог там ниже верхней кромки не-дублей намеренно: поиск по --id ничего
///    не запрещает, а показывает список, и лишняя строка полезнее пропущенной.
///
/// ПОЧЕМУ 0.75, А НЕ СЕРЕДИНА ЗАЗОРА (0.7076)
///
/// Здесь стояло «промах в сторону лишней остановки дешевле промаха в сторону
/// дубля» — и это не следовало из замера, а требовало порога НИЖЕ середины.
/// Текст и число тянули в разные стороны; правильным оказалось число, а не
/// объяснение.
///
/// Концы зазора смещены выборкой ПО-РАЗНОМУ, и в этом всё дело.
/// · Нижний конец (0.7795) держат пересказы, которые я писал сам, нарочно
///   уводя формулировки в сторону. Живой дубль так не выглядит: его пишет
///   агент по тому же шаблону и тем же словарём, что и оригинал, — он ляжет
///   ВЫШЕ, и порог поймает его тем более. То есть измеренный пол дублей — это
///   пессимистичная оценка, запас там больше, чем 0.0295.
/// · Верхний конец (0.6357) держит человеческая тонкая пара. А живой поток —
///   это тикеты, порождённые по одному шаблону («сделай X в модуле Y,
///   воспроизведи так-то»), и они уплотнят верх не-дублей СИЛЬНЕЕ, чем моя
///   выборка. Этот конец поедет вверх, и поедет именно с ростом объёма.
///
/// Поэтому порог стоит в верхней части зазора: опасность растёт со стороны
/// ложных остановок, а не пропусков. Цена признаётся честно: невиданный дубль,
/// севший между 0.75 и 0.7795, пройдёт молча. Он остаётся находимым через
/// ntk_similar, а вот лавина ложных отказов на воркспейсе в тысячи тикетов
/// сама себя не вылечит.
///
/// Тридцать тикетов — не весь свет, и обе оценки выше суть предположения о
/// том, куда поедут концы. Поэтому в журнал отказа пишется верхняя оценка:
/// вторую итерацию порога считать по накопленному на живом объёме, а не по
/// третьему предположению подряд.
pub const NEAR_DUPLICATE: f64 = 0.75;

/// Сколько похожих показываем. Больше пяти читать никто не станет.
pub const SIMILAR_LIMIT: usize = 5;

/// Похожий тикет: оценка из Qdrant, всё остальное — из SQL.
#[derive(Debug, serde::Serialize)]
pub struct Similar {
    pub id: String,
    pub score: f64,
    pub title: String,
    pub status: String,
    pub project: Option<String>,
}

/// Похожие на данный текст тикеты воркспейса.
///
/// Выдача СВЕРЯЕТСЯ с SQL и там же наполняется: точка в Qdrant могла отстать —
/// тикет удалили или переписали, а вектор ещё старый. Отдать заголовок из
/// нагрузки точки значило бы показывать то, чего уже нет, и с уверенным видом.
/// Поэтому Qdrant отвечает только «на кого смотреть», а что показать —
/// решает база.
pub async fn similar(
    v: &Vector,
    pool: &deadpool_postgres::Pool,
    ws: &str,
    title: &str,
    body: &str,
    limit: usize,
    min_score: f64,
    filter: Option<&crate::filter::Bound>,
) -> Result<Vec<Similar>> {
    let input = crate::write::embed_input(title, body);
    if input.trim().is_empty() {
        return Ok(Vec::new());
    }
    let vector = v.embed(&input).await?;
    // Статус отбирается в SQL, а не в Qdrant, и это не лень.
    //
    // Нагрузка точки могла бы нести статус — и врала бы: статус меняется каждый
    // день, а точка переписывается только при смене ТЕКСТА. Отбор по ней
    // прятал бы тикеты, которые давно открыли заново, и показывал бы закрытые
    // как открытые. Поэтому Qdrant отвечает «на кого смотреть», а годится ли
    // он — решает база.
    //
    // Цена — просить с запасом: отбор срежет часть выдачи, и без запаса вместо
    // пяти вернулось бы два. Пятикратный, с потолком, чтобы запрос не разрастался.
    let ask = if filter.is_some() { (limit * 5).min(100) } else { limit };
    let hits = v.search(vector, ws, ask, min_score).await?;
    if hits.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<String> = hits.iter().map(|(id, _)| id.clone()).collect();
    let mut c = pool.get().await?;
    let tx = crate::db::begin(&mut c, ws).await?;
    // Отбор — тот же, что у списка и обхода, из общего модуля. Свой набор
    // условий здесь означал бы, что «тег infra» в find и в ls понимаются
    // по-разному, и узнать об этом можно было бы только случайно.
    let mut sql = String::from(
        "select id, title, status, project_id from tickets
          where id = any($1) and deleted_at is null",
    );
    let mut args: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = vec![&ids];
    if let Some(b) = filter {
        crate::filter::apply(&mut sql, &mut args, b);
    }
    let rows = tx.query(&sql, &args).await?;
    tx.commit().await.ok();

    let mut live: std::collections::HashMap<String, (String, String, Option<String>)> =
        std::collections::HashMap::new();
    for r in &rows {
        live.insert(r.get(0), (r.get(1), r.get(2), r.get(3)));
    }
    // Порядок оставляем от Qdrant — он по убыванию похожести, а SQL про
    // похожесть ничего не знает.
    Ok(hits
        .into_iter()
        .filter_map(|(id, score)| {
            let (title, status, project) = live.remove(&id)?;
            Some(Similar { id, score, title, status, project })
        })
        // Запас, взятый ради отбора, наружу не отдаём.
        .take(limit)
        .collect())
}

/// Сверка Qdrant с SQL по СОСТАВУ, а не по числу.
///
/// Совпадение количеств ничего не доказывает: один призрак и один
/// непроиндексированный тикет дают то же число. Проверено на живом — в
/// воркспейсе test точек было 29 при 28 отметках, и лишней оказалась точка
/// тикета, СТРОКИ которого в базе уже нет: он был удалён физически ещё до
/// того, как долг научился переживать удаление тикета. Долга нет, поднять
/// нечем, сам по себе такой призрак не уйдёт никогда — он и остался бы в
/// выдаче поиска навсегда.
///
/// Направления два, и оба обязательны: точка без живого тикета удаляется,
/// живой тикет без точки получает долг.
pub async fn reconcile(
    v: &Vector,
    pool: &deadpool_postgres::Pool,
    ws: &str,
) -> Result<(usize, usize)> {
    let points = v.all_points(ws).await?;

    let mut c = pool.get().await?;
    let tx = crate::db::begin(&mut c, ws).await?;
    // Отпечаток считает та же функция, что и триггер засева: своей копии
    // выражения здесь больше нет — разойдись они хоть в символе, сверка
    // объявила бы устаревшим весь воркспейс и переиндексировала бы его каждые
    // шесть часов, за деньги.
    let rows = tx
        .query(
            "select id, uuid::text, vector_input_sha(title, body)
               from tickets where deleted_at is null",
            &[],
        )
        .await?;
    let live: std::collections::HashMap<String, (String, String)> =
        rows.iter().map(|r| (r.get(0), (r.get(1), r.get(2)))).collect();

    // Устарела точка или нет — вопрос не только про текст.
    //
    // Долг нужен и тем, у кого точки нет вовсе, и тем, у кого она отстала. А
    // отстать можно тремя способами, и наличием ни один из них не виден:
    // текст переписали, сменилась модель, сменился предел обрезки. Последние
    // два опаснее: sha остаётся верным, точка выглядит свежей, а индекс уже
    // смешанный — оценки в нём несравнимы, и поиск похожих врёт молча.
    let mut stale: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut wrong_model = 0usize;
    for p in &points {
        let Some((_, want)) = live.get(&p.ticket) else { continue };
        // Две причины считаются НЕЗАВИСИМО: точка, разошедшаяся и по тексту, и
        // по модели, должна попасть в обе цифры. Через else if она попадала бы
        // только в первую, и в журнале смена модели выглядела бы как обычная
        // правка текста — то есть как рутина, а не как смешанный индекс.
        let text_moved = p.sha.as_deref() != Some(want.as_str());
        let model_moved = p.model.as_deref() != Some(MODEL)
            || p.input_max != Some(crate::write::EMBED_INPUT_MAX);
        if model_moved {
            wrong_model += 1;
        }
        if text_moved || model_moved {
            stale.insert(p.ticket.as_str());
        }
    }
    let have: std::collections::HashSet<&str> = points.iter().map(|p| p.ticket.as_str()).collect();

    let mut seeded = 0usize;
    for (id, (uuid, _)) in &live {
        if have.contains(id.as_str()) && !stale.contains(id.as_str()) {
            continue;
        }
        // Считаем ФАКТИЧЕСКИ поставленное, а не намерение: долг, уже лежащий
        // со свежим отпечатком, трогать незачем, и цифра в журнале иначе
        // завышалась бы на каждом проходе, читаясь как «расхождение не чинится».
        //
        // Счётчик попыток сбрасываем: долг, вышедший из очереди по пяти
        // неудачам, иначе не поднимет никто, и сверка нашла бы расхождение,
        // ничего с ним не сделав.
        let n = tx
            .execute(
                "insert into vector_debt (ticket_id, uuid, op, input_sha, attempts, queued_at)
                 values ($1, $2, 'upsert', null, 0, now())
                 on conflict (ticket_id) do update
                    set attempts = 0, last_error = null
                  where vector_debt.attempts > 0",
                &[id, uuid],
            )
            .await?;
        seeded += n as usize;
    }
    tx.commit().await?;
    drop(c);

    let mut removed = 0usize;
    for p in &points {
        match live.get(&p.ticket) {
            // Тикета нет — призрак.
            None => {}
            // Тикет есть, но точка не его.
            //
            // Так бывает, когда тикет удалили физически и завели заново с тем
            // же идентификатором: uuid у него новый, и слив запишет НОВУЮ
            // точку, а старая останется навсегда — по составу она не призрак
            // (тикет-то жив) и по отпечатку лишь «отстала», сколько её ни
            // переиндексируй. Единственный призрак, которого сверка видела и
            // не убирала.
            Some((uuid, _)) if uuid != &p.id => {}
            Some(_) => continue,
        }
        v.delete_point(&p.id).await?;
        removed += 1;
        tracing::info!(
            workspace = %ws, ticket = %p.ticket, point = %p.id,
            "убрал точку без живого тикета"
        );
    }

    if !stale.is_empty() {
        tracing::info!(
            workspace = %ws, stale = stale.len(), wrong_model,
            "точки отстали от текущего текста или модели"
        );
    }
    // Канарейка на разъехавшиеся выражения отпечатка.
    //
    // Массовое «устарело» при спокойном воркспейсе — это не работа, а поломка:
    // так выглядит расхождение между SQL и Rust. Иначе оно видно только по
    // всплескам счёта раз в шесть часов, то есть по счёту от провайдера.
    //
    // Порогов два, и второй нужен именно из-за первого. Пол в восемь стоит
    // против ложных тревог: три точки, все три поправили при лежащем сливе —
    // это честное «устарело всё», и error по нему был бы шумом. Но он же
    // делает дрейф невидимым на маленьком воркспейсе. Поэтому «устарели ВСЕ
    // разом» — тревога при ЛЮБОМ размере: при живом сливе так не бывает, долги
    // разбираются за секунды, и полное расхождение означает либо дрейф
    // выражений, либо мёртвый слив. И то и другое стоит громкой строки.
    let all_stale = points.len() >= 2 && stale.len() == points.len();
    if all_stale || (points.len() >= 8 && stale.len() * 2 > points.len()) {
        tracing::error!(
            workspace = %ws, stale = stale.len(), points = points.len(),
            "устарело больше половины точек — похоже, разошлись выражения отпечатка \
             или встал слив, а не изменились тикеты"
        );
    }
    Ok((removed, seeded))
}

/// Сверка по расписанию.
///
/// Раз в шесть часов, а не при каждом пробуждении: она обходит ВСЕ точки
/// воркспейса, и делать это на каждую запись значило бы платить обходом за
/// правку одного тикета. Первая — через минуту после старта, чтобы не мешать
/// разбору долга, оставшегося с прошлого запуска.
pub fn reconcile_ticks(state: &Arc<crate::App>) {
    let Some(v) = state.vector.clone() else { return };
    let pool = state.pool.clone();
    let st = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        loop {
            match enabled_workspaces(&pool).await {
                Ok(list) => {
                    for ws in list {
                        match reconcile(&v, &pool, &ws).await {
                            Ok((0, 0)) => {}
                            Ok((removed, seeded)) => {
                                tracing::warn!(
                                    workspace = %ws, removed, seeded,
                                    "сверка нашла расхождение"
                                );
                                if seeded > 0 {
                                    wake(&st);
                                }
                            }
                            Err(e) => tracing::warn!(error = %e, workspace = %ws, "сверка не удалась"),
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %e, "сверка: воркспейсы не перечислились"),
            }
            tokio::time::sleep(Duration::from_secs(6 * 60 * 60)).await;
        }
    });
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
    // Читаем состояние тикета под ролью воркспейса — и отпускаем соединение
    // ДО отправки к провайдеру. Раньше оно жило до конца функции: одна задача
    // держала соединение и просила второе, а отправка занимает до 18 секунд.
    // При четырёх задачах из шестнадцати соединений это проходило, при восьми
    // дало бы гарантированный дедлок пула — мина ровно на «подниму
    // параллельность».
    let read = {
        let mut client = pool.get().await.context("the database is unavailable")?;
        let tx = crate::db::begin(&mut client, ws).await?;
        let row = tx
            .query_opt(
                "select uuid::text, project_id, title, body, deleted_at is not null
                   from tickets where id = $1",
                &[&ticket_id],
            )
            .await?;
        tx.commit().await.ok();
        row.map(|r| {
            (
                r.get::<_, String>(0),
                r.get::<_, Option<String>>(1),
                r.get::<_, String>(2),
                r.get::<_, String>(3),
                r.get::<_, bool>(4),
            )
        })
    };

    let (uuid, project, title, body, deleted) = match read {
        Some(t) => t,
        // Тикета нет вовсе. Раньше здесь долг просто снимался, и точка
        // оставалась в Qdrant навсегда: uuid жил только в удалённой строке.
        // Теперь его несёт сам долг, поэтому призрака можно снести.
        None => {
            match &debt_uuid {
                Some(u) => v.delete_point(u).await?,
                // После 024 недостижимо: uuid пишут все пути. Но если однажды
                // окажется достижимо, призрак останется в Qdrant навсегда и
                // найти его будет нечем — поэтому говорим вслух, а не молчим.
                None => tracing::warn!(
                    ticket = %ticket_id,
                    workspace = %ws,
                    "долг без uuid на исчезнувший тикет: точка, если была, осталась"
                ),
            }
            clear_debt(pool, ws, ticket_id).await?;
            return Ok(());
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
                // Записанное в Qdrant фиксируем всегда: это правда о том, что
                // там лежит, независимо от судьбы долга.
                tx.execute(
                    "insert into vector_index_state (ticket_id, indexed_sha, indexed_at)
                     values ($1, $2, now())
                     on conflict (ticket_id) do update
                        set indexed_sha = excluded.indexed_sha, indexed_at = now()",
                    &[&ticket_id, &sha],
                )
                .await?;
                // А долг снимаем ТОЛЬКО если он всё ещё тот, который мы взяли.
                //
                // Безусловное удаление здесь было настоящей дырой, и хуже
                // устаревшего вектора: правка, зафиксированная между этим
                // SELECT и этим DELETE, ставит новый долг — и DELETE съедал
                // именно его. Тикет оставался неиндексирован МОЛЧА, и поднять
                // его было нечем: долга нет, будить нечего, «следующий проход»
                // не случится никогда. READ COMMITTED тут не защищает: DELETE
                // видит строки заново, а не снимок начала транзакции.
                //
                // Сверяем с тем, что прочитали при захвате, а не с посчитанным:
                // долг с пустым отпечатком иначе не снялся бы никогда и крутил
                // бы платную отправку по кругу.
                let cleared = tx
                    .execute(
                        "delete from vector_debt
                          where ticket_id = $1 and input_sha is not distinct from $2",
                        &[&ticket_id, &debt_sha],
                    )
                    .await?;
                if cleared == 0 {
                    tracing::info!(
                        ticket = %ticket_id,
                        workspace = %ws,
                        "долг обновился за время отправки — оставляю на следующий проход"
                    );
                }
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
    let client = pool.get().await.context("the database is unavailable")?;
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
                        // Отказ не откатывает SQL и не теряет долг. Но попытку
                        // засчитываем ТОЛЬКО когда виноват вход: иначе минутный
                        // обморок провайдера уводил бы в мёртвые всю очередь
                        // разом, а мёртвый долг — это вечно старая точка, по
                        // которой потом ищут похожих.
                        let input_fault = e.downcast_ref::<InputFault>().is_some();
                        tracing::warn!(
                            error = %e, ticket = %id, workspace = %ws, counted = input_fault,
                            "долг не слился"
                        );
                        if input_fault {
                            let _ = bump_attempt(&pool, &ws, &id, &e.to_string()).await;
                        }
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
    let now: i32 = tx
        .query_one(
            "update vector_debt set attempts = attempts + 1, last_error = $2
              where ticket_id = $1 returning attempts",
            &[&id, &short],
        )
        .await?
        .get(0);
    tx.commit().await?;
    // Пятая попытка — это не «ещё одна неудача», а выход из очереди. Без
    // отдельной строки она в журнале неотличима от четвёртой, и долг лежит
    // молча: сверка потом «находит расхождение» и ничего с ним не делает.
    if now >= 5 {
        tracing::warn!(
            ticket = %id, workspace = %ws, attempts = now, error = %short,
            "долг вышел из очереди: больше не берётся, ждёт сверки или правки текста"
        );
    }
    Ok(())
}

/// Пробуждение. Слив один на процесс, но ни одно пробуждение не теряется.
///
/// Голого флага «идёт» тут не хватало. Запись, пришедшая между последним
/// `take_batch` и снятием флага, видела «идёт» и уходила молча, а слив уже
/// ничего не смотрел — долг оставался лежать до следующего события. Поэтому
/// флага два: `draining` говорит, что задача есть, `vector_pending` — что
/// появилась работа. Задача крутится, пока `vector_pending` снимается взведённым,
/// а после освобождения `draining` ещё раз смотрит на него и при нужде забирает
/// право обратно.
pub fn wake(state: &Arc<crate::App>) {
    use std::sync::atomic::Ordering::SeqCst;
    let Some(v) = state.vector.clone() else { return };
    // Взводим ДО захвата: иначе между захватом и взводом остаётся та же щель.
    state.vector_pending.store(true, SeqCst);
    if state.draining.swap(true, SeqCst) {
        return;
    }
    let pool = state.pool.clone();
    let st = state.clone();
    tokio::spawn(async move {
        loop {
            while st.vector_pending.swap(false, SeqCst) {
                drain(v.clone(), pool.clone()).await;
            }
            st.draining.store(false, SeqCst);
            // Освободили — и смотрим ещё раз. Если работа появилась в эту
            // щель, её `wake` ушёл ни с чем; забираем право назад. Если право
            // уже взял кто-то другой, он сам и разберёт: его `wake` взвёл флаг.
            if !st.vector_pending.load(SeqCst) || st.draining.swap(true, SeqCst) {
                break;
            }
        }
    });
}

/// Пробуждение по уведомлению из базы: включение векторизации.
///
/// Включают её руками, `UPDATE vector_policy`, — это событие в базе, а не
/// запрос к сервису, и процесс о нём не узнавал. Долг засевался триггером и
/// лежал до первой чужой записи или до перезапуска: «включили» и «пошло»
/// разъезжались на неопределённый срок.
///
/// Соединение здесь своё, не из пула: `LISTEN` живёт на соединении, а пул
/// вернёт его следующему запросу — подписка потерялась бы молча, и молчание
/// читалось бы как «уведомлений нет».
pub fn listen(state: &Arc<crate::App>, url: String) {
    if state.vector.is_none() {
        return;
    }
    let st = state.clone();
    tokio::spawn(async move {
        let mut pause = Duration::from_secs(1);
        loop {
            match subscribe(&st, &url).await {
                // Поток кончился без ошибки — база закрыла соединение. Подписка
                // при этом стояла, значит с базой всё в порядке: ждём секунду,
                // а не минуту, накопленную прошлыми отказами.
                Ok(()) => {
                    tracing::warn!("подписка на уведомления закрыта, переподключаюсь");
                    pause = Duration::from_secs(1);
                }
                Err(e) => tracing::warn!(error = %e, "подписка на уведомления отвалилась"),
            }
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(Duration::from_secs(60));
        }
    });
}

async fn subscribe(st: &Arc<crate::App>, url: &str) -> Result<()> {
    use futures_util::StreamExt;
    let (client, mut conn) = tokio_postgres::connect(url, tokio_postgres::NoTls).await?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();

    // Соединение обязано крутиться, пока мы говорим с базой.
    //
    // Первая версия делала LISTEN до того, как начинала читать сообщения, и
    // вставала намертво: запрос ждёт ответа, а ответ ждёт, чтобы его прочитали.
    // Молча — ни ошибки, ни строки в журнале, просто задача, которой больше
    // нет. Ровно тот случай, когда «ничего не написано» читается как «всё
    // хорошо»: поймал только по отсутствию строки об установленной подписке.
    let driver = tokio::spawn(async move {
        let mut msgs = futures_util::stream::poll_fn(move |cx| conn.poll_message(cx));
        while let Some(msg) = msgs.next().await {
            match msg {
                Ok(tokio_postgres::AsyncMessage::Notification(n)) => {
                    if tx.send(n.payload().to_string()).is_err() {
                        break;
                    }
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "соединение уведомлений оборвалось");
                    break;
                }
            }
        }
    });

    client.batch_execute("LISTEN ntk_vector").await?;
    tracing::info!("подписка на уведомления о векторизации установлена");
    // Пока связи не было, уведомления терялись безвозвратно — их никто не
    // хранит. Поэтому будим сразу после подписки, а не только по событию:
    // иначе включение, попавшее в разрыв, осталось бы незамеченным.
    wake(st);

    while let Some(payload) = rx.recv().await {
        tracing::info!(workspace = %payload, "включена векторизация");
        wake(st);
    }
    driver.abort();
    Ok(())
}

/// Повторный подход к тому, что не слилось с первого раза.
///
/// Отказ провайдера обрывает пачку и оставляет долг с непустым счётчиком
/// попыток. Разбудить его нечем: записи может не быть часами, а уведомление
/// приходит только на включение. Без этого редкая сетевая ошибка означала бы
/// «тикет не проиндексирован до следующей правки» — то есть, возможно, никогда.
pub fn retry_ticks(state: &Arc<crate::App>) {
    if state.vector.is_none() {
        return;
    }
    let st = state.clone();
    tokio::spawn(async move {
        let mut t = tokio::time::interval(Duration::from_secs(300));
        t.tick().await; // первый срабатывает сразу, а старт уже разбудил
        loop {
            t.tick().await;
            wake(&st);
        }
    });
}
