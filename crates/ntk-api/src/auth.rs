//! Разрешение ключа в личность и область доступа.
//!
//! Ключ принадлежит ПОЛЬЗОВАТЕЛЮ, а не воркспейсу: область берётся из
//! `core.user_workspaces`, поэтому ротация ключа её не меняет, а у человека
//! может быть несколько ключей. Отказ по умолчанию: нужен активный
//! пользователь, живой ключ и хотя бы одна строка доступа — иначе `core.
//! resolve_key` не вернёт ничего.

use axum::http::HeaderMap;
use sha2::{Digest, Sha256};

/// Кто пришёл и куда ему можно.
///
/// `kind` и `role` из `core.resolve_key` сюда намеренно не переносятся: пока
/// на них ничего не опирается, поле в структуре обещало бы работающий
/// механизм, которого нет. Появятся вместе с политиками для CAMLO.
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

/// Хранится и сверяется только sha256. Ключ высокоэнтропийный (240 бит),
/// поэтому соль не нужна, а простой хеш позволяет искать по индексу, а не
/// перебирать все строки на каждый запрос.
pub fn hash_key(key: &str) -> String {
    hex::encode(Sha256::digest(key.as_bytes()))
}

/// Достаёт ключ из заголовка. Принимаем только `Authorization: Bearer …` —
/// ключ в query-параметре осел бы в логах Caddy и в истории браузера.
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

/// Префикс токена удалённого MCP. Разбор по префиксу, а не перебор обеих
/// таблиц: иначе каждый неверный ключ стоил бы двух запросов, а «ключ» и
/// «токен» смешались бы в отчётах об отказах.
pub const OAUTH_ACCESS_PREFIX: &str = "ntkat_";

pub async fn resolve(
    client: &deadpool_postgres::Client,
    key: &str,
) -> anyhow::Result<Option<Actor>> {
    // Личность одна, способов её предъявить два: долгий ключ (CLI, флот) и
    // короткоживущий токен OAuth (Claude Desktop по HTTP). Разводить их на два
    // пути авторизации значит однажды закрыть дыру в одном и оставить в другом.
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
    // Отмечаем использование. Без этого «живой» и «мёртвый» ключ выглядят
    // одинаково, и решение об отзыве принимается вслепую — я на этом уже
    // ошибся, отзывая ключ по признаку, которого не существовало.
    //
    // Пишем не на каждый запрос, а раз в час: точность до часа отвечает на
    // вопрос «пользуются ли им», а запись на каждый вызов превратила бы
    // чтение в запись на самом горячем пути.
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
