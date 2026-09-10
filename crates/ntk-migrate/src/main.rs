//! Раннер миграций. Применяет объекты воркспейса в каждую схему воркспейса.
//!
//! Схемы и роли заводит серверная часть; здесь только объекты внутри уже
//! существующих схем. Список схем берётся из `core.workspaces`, а не из
//! аргументов: воркспейс, забытый в командной строке, остался бы без таблиц и
//! выяснилось бы это в проде.
//!
//! Запускать ОБЯЗАТЕЛЬНО от роли `ntk_admin`: под неё выданы
//! `ALTER DEFAULT PRIVILEGES`, поэтому созданная ею таблица сразу получает
//! права для `ntk_ws_*`. От `postgres` объекты создадутся, а права — нет, и это
//! обнаружится не здесь, а при первом запросе сервиса.

use anyhow::{Context, Result, bail};
use tokio_postgres::{Client, NoTls};

/// Файлы вшиты в бинарь: развёртыванию не нужен каталог `sql/` рядом, а
/// применённая миграция не может разойтись с той, что лежит на диске.
const MIGRATIONS: &[(&str, &str)] = &[
    ("001_init", include_str!("../../../sql/001_init.sql")),
    ("002_seed", include_str!("../../../sql/002_seed.sql")),
    ("003_id_case", include_str!("../../../sql/003_id_case.sql")),
    ("004_trigger_schema", include_str!("../../../sql/004_trigger_schema.sql")),
    ("005_id_shape", include_str!("../../../sql/005_id_shape.sql")),
    ("006_closed_at_honest", include_str!("../../../sql/006_closed_at_honest.sql")),
    ("007_current_status_at", include_str!("../../../sql/007_current_status_at.sql")),
    ("008_started_at_honest", include_str!("../../../sql/008_started_at_honest.sql")),
    ("009_soft_delete", include_str!("../../../sql/009_soft_delete.sql")),
    ("010_status_mark_on_insert", include_str!("../../../sql/010_status_mark_on_insert.sql")),
    ("011_project_index", include_str!("../../../sql/011_project_index.sql")),
    ("012_status_created_index", include_str!("../../../sql/012_status_created_index.sql")),
    ("013_release_per_platform", include_str!("../../../sql/013_release_per_platform.sql")),
    ("014_oauth_server", include_str!("../../../sql/014_oauth_server.sql")),
    ("015_walk_state", include_str!("../../../sql/015_walk_state.sql")),
    ("016_modules", include_str!("../../../sql/016_modules.sql")),
    ("017_module_lifecycle", include_str!("../../../sql/017_module_lifecycle.sql")),
    ("018_write_limits", include_str!("../../../sql/018_write_limits.sql")),
    ("019_vector_policy", include_str!("../../../sql/019_vector_policy.sql")),
    ("020_vector_debt", include_str!("../../../sql/020_vector_debt.sql")),
    ("021_vector_enable_seeds", include_str!("../../../sql/021_vector_enable_seeds.sql")),
    ("022_vector_seed_schema_fix", include_str!("../../../sql/022_vector_seed_schema_fix.sql")),
    ("023_vector_debt_survives_ticket", include_str!("../../../sql/023_vector_debt_survives_ticket.sql")),
    ("024_vector_seed_carries_uuid", include_str!("../../../sql/024_vector_seed_carries_uuid.sql")),
    ("025_vector_enable_notifies", include_str!("../../../sql/025_vector_enable_notifies.sql")),
    ("026_vector_input_sha_function", include_str!("../../../sql/026_vector_input_sha_function.sql")),
    ("027_free_target_statuses", include_str!("../../../sql/027_free_target_statuses.sql")),
    ("028_vector_stop_policy", include_str!("../../../sql/028_vector_stop_policy.sql")),
    ("029_drop_unused_meta_index", include_str!("../../../sql/029_drop_unused_meta_index.sql")),
];

#[tokio::main]
async fn main() -> Result<()> {
    // Номера обязаны быть уникальны.
    //
    // Два файла с номером 010 уже разошлись в дереве: применились оба, но
    // порядок между ними неопределён, а зависимая пара сломалась бы молча —
    // одна миграция ждала бы того, чего вторая ещё не сделала. Проверка стоит
    // пяти строк и снимает целый класс.
    {
        let mut seen = std::collections::HashSet::new();
        for (name, _) in MIGRATIONS {
            let num = name.split('_').next().unwrap_or(name);
            if !seen.insert(num) {
                anyhow::bail!("две миграции с номером {num}: порядок между ними не определён");
            }
        }
    }

    let url = std::env::var("DATABASE_URL")
        .context("DATABASE_URL не задан — строка подключения администратора лежит в /etc/ntk/admin.env")?;

    let (client, connection) = tokio_postgres::connect(&url, NoTls)
        .await
        .context("не удалось подключиться к базе")?;
    tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("соединение с базой разорвано: {e}");
        }
    });

    // Кодировка базы обязана быть UTF8.
    //
    // От неё зависит равенство двух вычислений одного отпечатка: триггер засева
    // считает sha256 от convert_to(..., 'UTF8'), а сервис — от байтов Rust-строки,
    // которые всегда UTF-8. В базе с другой кодировкой convert_to дало бы другие
    // байты на любом не-ASCII символе, отпечатки разошлись бы, и всё
    // переиндексировалось бы вечно — молча, потому что оба вычисления «работают».
    // Проверку предложил glm-ntk-reviewer как единственную оставшуюся предпосылку.
    let enc: String = client.query_one("show server_encoding", &[]).await?.get(0);
    if enc != "UTF8" {
        bail!(
            "кодировка базы {enc}, а нужна UTF8: иначе отпечаток входа для \
             векторизации считается по-разному в SQL и в сервисе, и индекс \
             переписывается бесконечно"
        );
    }

    let role: String = client.query_one("select current_user", &[]).await?.get(0);
    if role != "ntk_admin" {
        bail!(
            "миграции должны выполняться от ntk_admin, а не от {role}: \
             права для ntk_ws_* выдаются через ALTER DEFAULT PRIVILEGES этой роли, \
             и под другой ролью таблицы создадутся без них"
        );
    }

    let schemas = workspace_schemas(&client).await?;
    if schemas.is_empty() {
        bail!("в core.workspaces нет ни одного воркспейса — сначала заводится схема, потом объекты");
    }

    let mut applied_total = 0usize;
    for schema in &schemas {
        applied_total += apply_to_schema(&client, schema).await?;
    }

    if applied_total == 0 {
        println!("нечего применять: все схемы ({}) уже на последней миграции", schemas.len());
    }
    Ok(())
}

async fn workspace_schemas(client: &Client) -> Result<Vec<String>> {
    let rows = client
        .query("select schema_name from core.workspaces order by schema_name", &[])
        .await
        .context("не удалось прочитать core.workspaces")?;
    Ok(rows.iter().map(|r| r.get::<_, String>(0)).collect())
}

async fn apply_to_schema(client: &Client, schema: &str) -> Result<usize> {
    // Имя схемы приходит из базы, а не от пользователя, но в DDL его всё равно
    // нельзя подставить параметром — только идентификатором, поэтому оно
    // проверяется на форму перед склейкой.
    if !schema.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') {
        bail!("недопустимое имя схемы в core.workspaces: {schema:?}");
    }

    client
        .batch_execute(&format!(
            "create table if not exists {schema}._migrations (
               name        text primary key,
               applied_at  timestamptz not null default now()
             )"
        ))
        .await
        .with_context(|| format!("не удалось создать {schema}._migrations"))?;

    let done: Vec<String> = client
        .query(&format!("select name from {schema}._migrations"), &[])
        .await?
        .iter()
        .map(|r| r.get::<_, String>(0))
        .collect();

    let mut applied = 0usize;
    for (name, sql) in MIGRATIONS {
        if done.iter().any(|d| d == name) {
            continue;
        }
        // Каждая миграция целиком в транзакции: половина применённой миграции
        // хуже, чем непримененная — вторую видно, первую нет.
        client.batch_execute("begin").await?;
        let result = async {
            client.batch_execute(&format!("set local search_path to {schema}, core, public")).await?;
            client.batch_execute(sql).await?;
            client
                .execute(
                    &format!("insert into {schema}._migrations (name) values ($1)"),
                    &[name],
                )
                .await?;
            Ok::<_, tokio_postgres::Error>(())
        }
        .await;

        match result {
            Ok(()) => {
                client.batch_execute("commit").await?;
                println!("{schema}: применена {name}");
                applied += 1;
            }
            Err(e) => {
                client.batch_execute("rollback").await.ok();
                return Err(anyhow::Error::new(e))
                    .with_context(|| format!("{schema}: миграция {name} не применена, откат выполнен"));
            }
        }
    }
    Ok(applied)
}

#[cfg(test)]
mod registry_tests {
    use super::MIGRATIONS;

    /// Файл в sql/ обязан быть В СПИСКЕ.
    ///
    /// Миграции вшиты include_str! по явному перечислению, и это правильно:
    /// порядок применения — решение, а не результат сортировки каталога. Но у
    /// такого способа есть тихий отказ: новый файл просто не применяется, а
    /// раннер бодро сообщает «все схемы на последней миграции». Наступил на это
    /// сам, добавив 018 и получив «нечего применять» при отсутствующей таблице.
    #[test]
    fn every_sql_file_is_registered() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sql");
        let mut missing = Vec::new();
        for e in std::fs::read_dir(&dir).expect("каталог sql/ не читается") {
            let name = e.expect("запись каталога").file_name().to_string_lossy().to_string();
            let Some(stem) = name.strip_suffix(".sql") else { continue };
            if !MIGRATIONS.iter().any(|(id, _)| *id == stem) {
                missing.push(stem.to_string());
            }
        }
        missing.sort();
        assert!(
            missing.is_empty(),
            "эти файлы sql/ не зарегистрированы и НЕ ПРИМЕНЯТСЯ: {missing:?}"
        );
    }

    /// И обратно: в списке нет того, чего нет на диске. Такое не собралось бы,
    /// но проверка держит список и каталог в одном соответствии с двух сторон.
    #[test]
    fn the_registry_is_ordered_and_unique() {
        let ids: Vec<&str> = MIGRATIONS.iter().map(|(id, _)| *id).collect();
        let mut sorted = ids.clone();
        sorted.sort_unstable();
        assert_eq!(ids, sorted, "список миграций не по порядку: {ids:?}");
        let mut uniq = sorted.clone();
        uniq.dedup();
        assert_eq!(sorted.len(), uniq.len(), "в списке миграций есть повтор");
    }
}
