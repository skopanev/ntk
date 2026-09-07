//! Самообновление клиента.
//!
//! Человек ставит DMG один раз, дальше клиент обновляется сам. Отсюда же
//! главное требование: скачанное проверяется ДО того, как станет исполняемым
//! файлом на его машине. Обновление без проверки — канал доставки чего угодно,
//! и самый удобный: он уже имеет право переписать бинарь.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Write;


#[derive(Deserialize)]
struct Release {
    version: String,
    url: String,
    sha256: String,
}

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// Платформа этой сборки. Определяется на компиляции, а не угадывается:
/// сервер обязан отдать бинарь именно для неё. Раньше платформа была зашита на
/// сервере, и Linux-клиенту предлагался macOS-бинарь — успешное обновление
/// сломало бы установку.
pub const PLATFORM: &str = concat!(env!("NTK_OS"), "-", env!("NTK_ARCH"));

async fn latest(base: &str) -> Result<Release> {
    let resp = reqwest::get(format!("{}/v1/version?platform={}", base.trim_end_matches('/'), PLATFORM))
        .await
        .context("сервис недоступен")?;

    // Отказ разбирается ДО тела: иначе сервер говорит человеку по делу, а он
    // видит «ответ о версии не разобрался — missing field version». Ровно так
    // и вышло, когда platform стал обязательным: клиенты постарше его не
    // слали, получали внятное объяснение и показывали вместо него мусор.
    let status = resp.status();
    let body = resp.text().await.context("ответ сервиса не дочитался")?;
    if !status.is_success() {
        let msg = serde_json::from_str::<serde_json::Value>(&body)
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(str::to_owned))
            .unwrap_or_else(|| body.trim().to_owned());
        bail!("сервис не отдал версию для {PLATFORM} ({status}): {msg}");
    }
    serde_json::from_str(&body).context("ответ о версии не разобрался")
}

/// Сравнение по частям, а не строкой: «0.5.10» больше «0.5.9», хотя строкой
/// меньше.
fn newer(a: &str, b: &str) -> bool {
    let p = |v: &str| -> Vec<u32> { v.split('.').map(|x| x.parse().unwrap_or(0)).collect() };
    p(a) > p(b)
}

/// Обновление по команде, а не само.
///
/// Автоматическую проверку сняли по решению владельца: она добавляла поход в
/// сеть к каждому запуску ради события, которое случается раз в неделю, и
/// давала лишний способ сломаться там, где человек просто смотрит тикеты.
pub async fn run(base: &str) -> Result<()> {
    let r = latest(base).await?;
    if !newer(&r.version, CURRENT) {
        println!("уже последняя: {CURRENT}");
        return Ok(());
    }
    println!("{CURRENT} → {}", r.version);
    apply(&r).await?;
    println!("готово: {}", r.version);
    // Работающий сервер MCP от подмены файла не меняется: процесс запущен
    // старым бинарником и живёт до конца сессии. Переспросить его список
    // инструментов бесполезно — он честно отдаст тот, что у него есть, и
    // выглядеть это будет как «новых параметров нет», а не как «нужен
    // перезапуск». Сказать это здесь дешевле, чем выяснять каждый раз заново.
    println!("перезапустите Claude — иначе он продолжит видеть прежний набор инструментов");
    Ok(())
}

async fn apply(r: &Release) -> Result<()> {
    let bytes = reqwest::get(&r.url).await.context("не скачалось")?.bytes().await?;

    let got = hex::encode(Sha256::digest(&bytes));
    if got != r.sha256 {
        bail!("контрольная сумма не сошлась: ожидали {}, получили {got}", r.sha256);
    }

    // Кладём рядом с текущим бинарём: подмена должна быть переименованием в
    // пределах одной файловой системы, иначе она не атомарна и можно остаться
    // с половиной файла.
    let me = std::env::current_exe().context("не понять, где я")?;
    let dir = me.parent().context("нет каталога")?;
    let tmp = dir.join(".ntk.new");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(&bytes)?;
        f.flush()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    }

    // Подпись проверяем ПОСЛЕ скачивания и ДО подмены. Контрольная сумма
    // говорит лишь, что файл дошёл целым: её значение пришло оттуда же, откуда
    // файл. Подпись говорит, КТО его собрал.
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("codesign")
            .args(["-v", "--strict"])
            .arg(&tmp)
            .output()
            .context("codesign не запустился")?;
        if !out.status.success() {
            let _ = std::fs::remove_file(&tmp);
            bail!(
                "подпись скачанного не прошла проверку — обновление отменено: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
    }

    std::fs::rename(&tmp, &me).context("не удалось заменить бинарь")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn versions_compare_by_number_not_by_text() {
        assert!(newer("0.5.10", "0.5.9"), "строкой 10 меньше 9, числом больше");
        assert!(newer("0.6.0", "0.5.99"));
        assert!(!newer("0.5.0", "0.5.0"));
        assert!(!newer("0.4.9", "0.5.0"));
    }
}
