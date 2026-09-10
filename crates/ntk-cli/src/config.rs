//! Client config: service address and key.
//!
//! There is deliberately no "default workspace". A forgotten `-W` used to send
//! a write quietly into someone else's workspace; now it is a refusal.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_url")]
    pub url: String,
    /// One key per person: the scope lives on the server, not in the key, so
    /// one covers every workspace you have.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

// A `rest` field lived here for the old JS client, which kept third-party
// tokens in the same file. Both are gone, so the field is too — and the next
// config write sweeps those stale tokens off the disk.

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
        return Ok(Config { url: default_url(), key: None });
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
    // The key is a secret: the file must not be world-readable.
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
        _ => bail!("no key — run: ntk login"),
    }
}

/// The workspace comes from the nearest `.ntkrc` up the tree, or from `-W`.
///
/// `.ntkrc` is JSON: `{"v":2,"workspace":"…","project":"…"}`. The first version
/// parsed it as `key = value` and found nothing: every command answered "no
/// workspace given" while it was written right there. Test against a real file.
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

/// The project from `.ntkrc` — same way, same reason.
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
    fn an_old_config_reads_and_loses_the_notion_leftovers() {
        // A working config from the JS version, third-party tokens and all.
        //
        // This test used to demand the OPPOSITE: that a write preserve foreign
        // fields, because a live JS client owned the same file. Dropped on
        // purpose along with the rest field. Rewritten rather than deleted —
        // otherwise nothing pins down that an old config still READS.
        let raw = r#"{
            "default_workspace": "default",
            "workspaces": { "ftk": { "token": "ntn_secret", "database_id": "db" } }
        }"#;
        let cfg: Config = serde_json::from_str(raw).expect("an old config must still read");
        assert!(cfg.key.is_none());
        assert_eq!(cfg.url, default_url());

        let written = serde_json::to_string(&cfg).unwrap();
        assert!(
            !written.contains("ntn_secret"),
            "токен Notion обязан уйти с диска при записи: {written}"
        );
        assert!(
            !written.contains("default_workspace"),
            "поля JS-клиента больше не сохраняются: {written}"
        );
    }
}
