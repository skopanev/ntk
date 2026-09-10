//! Database access. The main isolation property lives here too.
//!
//! `ntk_api` holds no rights on workspace schemas. It is merely a member of the
//! `ntk_ws_*` roles and is declared NOINHERIT, so the rights appear ONLY after
//! an explicit `SET LOCAL ROLE`. A filter forgotten in the code cannot open
//! somebody else's workspace: the database refuses. The property is checked by
//! `server/db/isolation-test.sh`, and before NOINHERIT it did not hold — a
//! member role inherits rights by default.

use anyhow::{bail, Result};
use deadpool_postgres::{Config as PgConfig, Pool, Runtime};
use tokio_postgres::NoTls;

pub fn pool(database_url: &str) -> Result<Pool> {
    let mut cfg = PgConfig::new();
    cfg.url = Some(database_url.to_string());

    // By default deadpool takes four connections per core, and there is one
    // core here. Four is a hidden ceiling: harmless today, binding as we grow,
    // and it would look like "the database is slow" — the queries would be
    // queueing for a connection, not for the database.
    //
    // Sixteen against Postgres's max_connections = 100: there is headroom, and
    // connections are cheaper than waiting.
    cfg.pool = Some(deadpool_postgres::PoolConfig {
        max_size: 16,
        ..Default::default()
    });
    Ok(cfg.create_pool(Some(Runtime::Tokio1), NoTls)?)
}

/// The workspace name comes from `core.workspaces`, but it is interpolated into
/// SQL as an identifier, so it is validated anyway: trusting the source is no
/// substitute for checking at the boundary.
fn valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 31
        && s.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// Opens a transaction, having entered the workspace role.
///
/// `SET LOCAL` rolls back with the transaction — a hard requirement when
/// connections are pooled: without `LOCAL` the role would stay on the
/// connection and leak into the next query, possibly somebody else's.
///
/// We return the transaction itself rather than take a closure: the
/// `AsyncFnOnce` version looked tidier, but its future cannot be proven `Send`,
/// and axum refused such a handler. Plainer is sturdier.
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
            assert!(!valid_ident(bad), "{bad:?} must be refused");
        }
    }

    #[test]
    fn real_workspace_names_pass() {
        for good in ["ftk", "acme", "ws_2"] {
            assert!(valid_ident(good), "{good:?} must pass");
        }
    }
}
