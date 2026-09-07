//! Снимок Notion: только чтение, ничего не меняется.
//!
//! Артефакт отката. Пока он не снят и не сверен, удалять Notion нельзя —
//! поэтому здесь важнее полнота, чем скорость, и любая недочитанная часть
//! помечается прямо в данных, а не молчит.

use anyhow::{Context, Result, bail};
use ntk_export::{blocks::blocks_to_text, notion::Notion};
use serde_json::{Value, json};
use std::io::Write;

/// Глубина вложенности, дальше которой снимок не идёт, и потолок запросов на
/// одно тело. Оба предела помечаются в данных: укороченное тело, читающееся
/// как целое, — ровно тот дефект, из-за которого `-d` уничтожал содержимое.
const MAX_DEPTH: usize = 4;
const BUDGET_PER_BODY: usize = 60;

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let workspace = args.next().unwrap_or_else(|| "default".into());
    let out_path = args.next().unwrap_or_else(|| format!("notion-{workspace}.jsonl"));

    let (token, database_id) = workspace_config(&workspace)?;
    let nt = Notion::new(token)?;
    let ds = nt.data_source_id(&database_id).await?;

    eprintln!("снимок воркспейса {workspace} → {out_path}");
    let pages = nt.all_pages(&ds).await?;
    eprintln!("страниц: {}", pages.len());

    let mut file = std::fs::File::create(&out_path)
        .with_context(|| format!("не удалось создать {out_path}"))?;
    let mut with_body = 0usize;
    let mut truncated = 0usize;

    for (i, page) in pages.iter().enumerate() {
        let page_id = page.get("id").and_then(Value::as_str).unwrap_or_default();
        let mut budget = BUDGET_PER_BODY;
        let tree = fetch_tree(&nt, page_id, 1, &mut budget).await?;
        let body = blocks_to_text(&tree, "");
        if !body.is_empty() {
            with_body += 1;
        }
        if budget == 0 {
            truncated += 1;
        }

        let record = json!({
            "page": page,
            "body": body,
            "body_complete": budget > 0,
        });
        writeln!(file, "{}", serde_json::to_string(&record)?)?;

        if (i + 1) % 25 == 0 {
            eprintln!("  {} / {}", i + 1, pages.len());
        }
    }

    eprintln!("готово: {} записей, тела у {}, недочитанных тел {}", pages.len(), with_body, truncated);
    if truncated > 0 {
        eprintln!("недочитанные тела помечены body_complete=false — импорт обязан на них споткнуться, а не принять");
    }
    Ok(())
}

/// Дерево блоков вглубь, с потолком запросов. Причина остановки пишется в сам
/// блок, чтобы рендер её показал.
async fn fetch_tree(nt: &Notion, block_id: &str, depth: usize, budget: &mut usize) -> Result<Vec<Value>> {
    if *budget == 0 {
        return Ok(Vec::new());
    }
    *budget -= 1;
    let mut children = nt.children(block_id).await?;

    for child in &mut children {
        let has_children = child.get("has_children").and_then(Value::as_bool) == Some(true);
        if !has_children {
            continue;
        }
        if depth >= MAX_DEPTH {
            child["elided"] = json!("depth");
            continue;
        }
        if *budget == 0 {
            child["elided"] = json!("budget");
            continue;
        }
        let id = child.get("id").and_then(Value::as_str).unwrap_or_default().to_string();
        let nested = Box::pin(fetch_tree(nt, &id, depth + 1, budget)).await?;
        if !nested.is_empty() {
            child["children"] = Value::Array(nested);
        }
    }
    Ok(children)
}

/// Токен и база берутся из того же конфига, которым пользуется сегодняшний
/// CLI: снимок должен читать ровно ту базу, что работает, а не ту, что кто-то
/// указал в аргументах по памяти.
fn workspace_config(name: &str) -> Result<(String, String)> {
    let path = dirs_config()?;
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("не читается {}", path.display()))?;
    let cfg: Value = serde_json::from_str(&raw).context("конфиг не разбирается")?;
    let ws = cfg.pointer(&format!("/workspaces/{name}"))
        .with_context(|| format!("в конфиге нет воркспейса {name}"))?;
    let token = ws.get("token").and_then(Value::as_str)
        .with_context(|| format!("у воркспейса {name} нет token"))?;
    let db = ws.get("database_id").and_then(Value::as_str)
        .with_context(|| format!("у воркспейса {name} нет database_id"))?;
    if token.is_empty() || db.is_empty() {
        bail!("у воркспейса {name} пустой token или database_id");
    }
    Ok((token.to_string(), db.to_string()))
}

fn dirs_config() -> Result<std::path::PathBuf> {
    let home = std::env::var("HOME").context("HOME не задан")?;
    Ok(std::path::Path::new(&home).join(".config/ntk/config.json"))
}
