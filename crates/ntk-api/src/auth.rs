//! Resolving a key into an identity and a scope.
//!
//! A key belongs to a USER, not to a workspace: the scope comes from
//! `core.user_workspaces`, so rotating a key does not change it, and one person
//! may hold several keys. Deny by default: it takes an active user, a live key
//! and at least one access row — otherwise `core.resolve_key` returns nothing.

use axum::http::HeaderMap;
use sha2::{Digest, Sha256};

/// Who turned up and where they are allowed.
///
/// `kind` and `role` from `core.resolve_key` are deliberately not carried here:
/// while nothing rests on them, a field in the struct would promise a working
/// mechanism that does not exist. They arrive together with the policies.
#[derive(Debug, Clone)]
pub struct Actor {
    pub user_id: String,
    pub workspaces: Vec<String>,
}

impl Actor {
    pub fn may_enter(&self, workspace: &str) -> bool {
        self.workspaces.iter().any(|w| w == workspace)
    }
}

/// Only the sha256 is stored and compared. The key is high-entropy (240 bits),
/// so no salt is needed, and a plain hash lets us search by index instead of
/// walking every row on each request.
pub fn hash_key(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

/// Pulls the key out of the header. Only `Authorization: Bearer …` is accepted
/// — a key in a query parameter would settle in Caddy's logs and in browser
/// history.
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// The prefix of a remote MCP token. Dispatch by prefix rather than trying both
/// tables: otherwise every wrong key would cost two queries, and "key" and
/// "token" would blur together in the refusal reports.
pub const OAUTH_ACCESS_PREFIX: &str = "ntkat_";

pub async fn resolve(
    client: &deadpool_postgres::Client,
    key: &str,
) -> anyhow::Result<Option<Actor>> {
    // One identity, two ways to present it: a long-lived key (CLI, fleet) and
    // a short-lived OAuth token (Claude Desktop over HTTP). Splitting them into
    // two authorisation paths means one day closing a hole in one and leaving
    // it open in the other.
    if key.starts_with(OAUTH_ACCESS_PREFIX) {
        return crate::mcp_oauth::actor_from_token(client, key).await;
    }
    let rows = client
        .query(
            "select user_id, kind, role, workspaces from core.resolve_key($1)",
            &[&hash_key(key)],
        )
        .await?;
    let Some(r) = rows.first() else {
        return Ok(None);
    };
    // Record the use. Without it a live key and a dead one look the same, and
    // a revocation decision is taken blind — I already got that wrong once,
    // revoking a key on the strength of a signal that did not exist.
    //
    // Written once an hour rather than on every request: hour precision answers
    // "is anyone using it", while writing on every call would turn a read into
    // a write on the hottest path.
    let _ = client
        .execute(
            "update core.user_keys set last_used_at = now()
              where key_hash = $1
                and (last_used_at is null or last_used_at < now() - interval '1 hour')",
            &[&hash_key(key)],
        )
        .await;

    Ok(Some(Actor {
        user_id: r.get(0),
        workspaces: r.get(3),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn hash_is_stable_and_not_the_key() {
        let h = hash_key("ntk_example");
        assert_eq!(h.len(), 64);
        assert_eq!(h, hash_key("ntk_example"));
        assert!(!h.contains("ntk_example"));
    }

    #[test]
    fn only_bearer_is_accepted() {
        let mut h = HeaderMap::new();
        h.insert("authorization", HeaderValue::from_static("Bearer ntk_abc"));
        assert_eq!(bearer(&h), Some("ntk_abc"));

        for bad in ["ntk_abc", "Basic ntk_abc", "Bearer ", "Bearer  "] {
            let mut h = HeaderMap::new();
            h.insert("authorization", HeaderValue::from_str(bad).unwrap());
            assert_eq!(bearer(&h), None, "«{bad}» не должен приниматься");
        }
        assert_eq!(bearer(&HeaderMap::new()), None);
    }

    #[test]
    fn reach_is_explicit() {
        let a = Actor { user_id: "skk".into(), workspaces: vec!["ftk".into()] };
        assert!(a.may_enter("ftk"));
        assert!(!a.may_enter("acme"));
    }
}
