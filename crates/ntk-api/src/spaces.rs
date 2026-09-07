//! Предподписанные ссылки на объекты Spaces.
//!
//! Подпись считается руками на `node:crypto`-эквиваленте из `ring`/`hmac` —
//! это HMAC-SHA256 по строке запроса, сорок строк. Тащить SDK ради одной
//! функции незачем: тот же довод, по которому бэкап обходится обычным curl.
//! Отличие от бэкапа только в форме — там подпись в заголовке, здесь в
//! параметрах, чтобы ссылку можно было просто отдать клиенту.

use anyhow::Result;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

use crate::config::Config;

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut m = <Hmac<Sha256>>::new_from_slice(key).expect("любой размер ключа годится");
    m.update(data.as_bytes());
    m.finalize().into_bytes().to_vec()
}

/// Ссылка на чтение объекта, живущая `ttl` секунд.
pub fn presign_get(cfg: &Config, object_key: &str, ttl: u32) -> Result<String> {
    presign(cfg, "GET", object_key, ttl)
}

/// Ссылка на запись. Клиент грузит байты НАПРЯМУЮ в Spaces, мимо дроплета:
/// на одном ядре и двух гигабайтах файл в полсотни мегабайт через сервис —
/// это его память и полоса на каждую операцию.
pub fn presign_put(cfg: &Config, object_key: &str, ttl: u32) -> Result<String> {
    presign(cfg, "PUT", object_key, ttl)
}

/// Ссылка для проверки существования объекта.
///
/// Отдельная от GET намеренно: подпись считается ПО МЕТОДУ, и ссылка,
/// подписанная для GET, на HEAD не годится. На этом и попались — загрузка
/// проходила, а проверка отвечала «объекта нет в хранилище».
pub fn presign_head(cfg: &Config, object_key: &str, ttl: u32) -> Result<String> {
    presign(cfg, "HEAD", object_key, ttl)
}

fn presign(cfg: &Config, method: &str, object_key: &str, ttl: u32) -> Result<String> {
    let (access, secret, bucket, region, endpoint) = (
        cfg.spaces_key.as_deref().unwrap_or_default(),
        cfg.spaces_secret.as_deref().unwrap_or_default(),
        cfg.spaces_bucket.as_deref().unwrap_or_default(),
        cfg.spaces_region.as_deref().unwrap_or("ams3"),
        cfg.spaces_endpoint.as_deref().unwrap_or("https://ams3.digitaloceanspaces.com"),
    );
    if access.is_empty() || secret.is_empty() {
        anyhow::bail!("ключи Spaces не заданы");
    }

    let now = jiff::Timestamp::now();
    let amz_date = now.strftime("%Y%m%dT%H%M%SZ").to_string();
    let date = now.strftime("%Y%m%d").to_string();
    let scope = format!("{date}/{region}/s3/aws4_request");

    let host = endpoint.trim_start_matches("https://").trim_end_matches('/');
    let path = format!("/{bucket}/{object_key}");

    let mut q = vec![
        ("X-Amz-Algorithm", "AWS4-HMAC-SHA256".to_string()),
        ("X-Amz-Credential", format!("{access}/{scope}")),
        ("X-Amz-Date", amz_date.clone()),
        ("X-Amz-Expires", ttl.to_string()),
        ("X-Amz-SignedHeaders", "host".to_string()),
    ];
    q.sort_by(|a, b| a.0.cmp(b.0));
    let query: String = q
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding::encode(k), urlencoding::encode(v)))
        .collect::<Vec<_>>()
        .join("&");

    let canonical = format!(
        "{method}\n{path}\n{query}\nhost:{host}\n\nhost\nUNSIGNED-PAYLOAD"
    );
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex::encode(Sha256::digest(canonical.as_bytes()))
    );

    let k = hmac(format!("AWS4{secret}").as_bytes(), &date);
    let k = hmac(&k, region);
    let k = hmac(&k, "s3");
    let k = hmac(&k, "aws4_request");
    let signature = hex::encode(hmac(&k, &to_sign));

    Ok(format!("https://{host}{path}?{query}&X-Amz-Signature={signature}"))
}
