//! Конфиг клиента: адрес сервиса и ключ на каждый воркспейс.
//!
//! Понятия «воркспейс по умолчанию» здесь нет намеренно. Раньше забытый `-W`
//! молча уводил запись в чужой воркспейс; теперь это отказ с объяснением.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_url")]
    pub url: String,
    /// Один ключ на пользователя: область доступа хранится на сервере, а не
    /// в ключе, поэтому одного достаточно на все свои воркспейсы.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Всё остальное, что лежит в файле, сохраняется как есть.
    ///
    /// Это не запас на будущее. Тот же файл сейчас принадлежит работающему
    /// JS-клиенту и держит `workspaces` с токенами Notion; без этого поля
    /// первый же `ntk login` переписал бы файл двумя строчками и оставил флот
    /// без доступа. Уйдёт вместе с Notion, не раньше.
    #[serde(flatten)]
    pub rest: serde_json::Map<String, serde_json::Value>,
}

fn default_url() -> String {
    "https://ntk.example.com".into()
}

pub fn path() -> Result<PathBuf> {
    let home = std::env::var("HOME").context("HOME не задан")?;
    Ok(PathBuf::from(home).join(".config/ntk/config.json"))
}

pub fn load() -> Result<Config> {
    let p = path()?;
    if !p.exists() {
        return Ok(Config { url: default_url(), key: None, rest: Default::default() });
    }
    let raw = std::fs::read_to_string(&p).with_context(|| format!("не читается {}", p.display()))?;
    Ok(serde_json::from_str(&raw).with_context(|| format!("{} не разбирается", p.display()))?)
}

pub fn save(cfg: &Config) -> Result<()> {
    let p = path()?;
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&p, serde_json::to_string_pretty(cfg)?)?;
    // Ключ — секрет: файл не должен быть читаем всей машиной.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

pub fn require_key(cfg: &Config) -> Result<&str> {
    match cfg.key.as_deref() {
        Some(k) if !k.is_empty() => Ok(k),
        _ => bail!("нет ключа — выполните: ntk login"),
    }
}

/// Воркспейс берётся из `.ntkrc` рядом или из `-W`. Догадок нет.
/// Воркспейс из `.ntkrc` ближайшего каталога вверх по дереву.
///
/// `.ntkrc` — это JSON: `{"v":2,"workspace":"…","project":"…"}`. Первая версия
/// разбирала его как `ключ = значение` и не находила ничего вообще: любая
/// команда в репозитории отвечала «не указан воркспейс», хотя он там был
/// записан. Проверять надо было на настоящем файле, а не на придуманном.
pub fn workspace_from_rc() -> Option<String> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let rc = dir.join(".ntkrc");
        if rc.exists() {
            if let Ok(text) = std::fs::read_to_string(&rc) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(ws) = v.get("workspace").and_then(|w| w.as_str()) {
                        if !ws.is_empty() {
                            return Some(ws.to_string());
                        }
                    }
                }
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Проект из `.ntkrc` — тем же способом и по той же причине.
pub fn project_from_rc() -> Option<String> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let rc = dir.join(".ntkrc");
        if rc.exists() {
            if let Ok(text) = std::fs::read_to_string(&rc) {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(p) = v.get("project").and_then(|p| p.as_str()) {
                        if !p.is_empty() {
                            return Some(p.to_string());
                        }
                    }
                }
            }
        }
        if !dir.pop() {
            return None;
        }
    }
}

#[cfg(test)]
mod rc_tests {
    #[test]
    fn ntkrc_is_json_not_key_value() {
        let dir = std::env::temp_dir().join(format!("ntkrc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(".ntkrc"), r#"{"v":2,"workspace":"acme","project":"lib"}"#).unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let ws = super::workspace_from_rc();
        let pr = super::project_from_rc();
        std::env::set_current_dir(prev).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(ws.as_deref(), Some("acme"), "воркспейс обязан читаться из JSON");
        assert_eq!(pr.as_deref(), Some("lib"));
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saving_keeps_what_the_old_client_needs() {
        // Форма рабочего конфига JS-версии: воркспейсы с токенами Notion.
        let raw = r#"{
            "default_workspace": "default",
            "workspaces": { "ftk": { "token": "ntn_secret", "database_id": "db" } }
        }"#;
        let cfg: Config = serde_json::from_str(raw).expect("старый конфиг обязан читаться");
        assert!(cfg.key.is_none());
        assert_eq!(cfg.url, default_url());

        let written = serde_json::to_string(&cfg).unwrap();
        assert!(written.contains("ntn_secret"), "токен Notion потерян при записи: {written}");
        assert!(written.contains("default_workspace"), "поле старого клиента потеряно: {written}");
    }
}
