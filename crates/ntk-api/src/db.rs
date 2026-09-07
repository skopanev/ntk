//! Доступ к базе. Здесь же держится главное свойство изоляции.
//!
//! `ntk_api` не имеет прав на схемы воркспейсов. Он лишь член ролей
//! `ntk_ws_*` и объявлен NOINHERIT, поэтому права появляются ТОЛЬКО после
//! явного `SET LOCAL ROLE`. Забытый фильтр в коде чужой воркспейс не откроет:
//! откажет база. Свойство проверяется `server/db/isolation-test.sh`, и до
//! NOINHERIT оно не выполнялось — роль-член наследует права по умолчанию.

use anyhow::{bail, Result};
use deadpool_postgres::{Config as PgConfig, Pool, Runtime};
use tokio_postgres::NoTls;

pub fn pool(database_url: &str) -> Result<Pool> {
    let mut cfg = PgConfig::new();
    cfg.url = Some(database_url.to_string());

    // По умолчанию deadpool берёт четыре соединения на ядро, а ядро здесь одно.
    // Четыре — это скрытый потолок, который не мешает сегодня и упрётся при
    // росте, причём выглядеть будет как «база тормозит»: запросы встанут в
    // очередь за соединением, а не за базой.
    //
    // Шестнадцать против max_connections = 100 у Postgres: запас есть, и
    // соединения дешевле, чем ожидание.
    cfg.pool = Some(deadpool_postgres::PoolConfig {
        max_size: 16,
        ..Default::default()
    });
    Ok(cfg.create_pool(Some(Runtime::Tokio1), NoTls)?)
}

/// Имя воркспейса приходит из `core.workspaces`, но подставляется в SQL как
/// идентификатор, поэтому проверяется всё равно: доверие к источнику не
/// заменяет проверку на границе.
fn valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 31
        && s.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Открывает транзакцию, войдя в роль воркспейса.
///
/// `SET LOCAL` откатывается вместе с транзакцией — обязательное условие при
/// пуле соединений: без `LOCAL` роль осталась бы на соединении и утекла бы в
/// следующий запрос, возможно чужой.
///
/// Возвращаем саму транзакцию, а не принимаем замыкание: вариант с
/// `AsyncFnOnce` выглядел аккуратнее, но его future не доказывается `Send`, и
/// axum отказывался принимать такой хендлер. Прямее — надёжнее.
pub async fn begin<'a>(
    client: &'a mut deadpool_postgres::Client,
    workspace: &str,
) -> Result<deadpool_postgres::Transaction<'a>> {
    if !valid_ident(workspace) {
        bail!("недопустимое имя воркспейса: {workspace:?}");
    }
    let tx = client.transaction().await?;
    tx.batch_execute(&format!(
        r#"SET LOCAL ROLE "ntk_ws_{workspace}"; SET LOCAL search_path = "{workspace}";"#
    ))
    .await?;
    Ok(tx)
}

#[cfg(test)]
mod tests {
    use super::valid_ident;

    #[test]
    fn identifiers_that_could_break_out_are_refused() {
        for bad in [
            "",
            "ftk; drop schema core cascade",
            r#"ftk" cascade --"#,
            "Ftk",
            "1ftk",
            "ftk-1",
            &"a".repeat(32),
        ] {
            assert!(!valid_ident(bad), "«{bad}» должен быть отвергнут");
        }
    }

    #[test]
    fn real_workspace_names_pass() {
        for good in ["ftk", "acme", "ws_2"] {
            assert!(valid_ident(good), "«{good}» должен проходить");
        }
    }
}
