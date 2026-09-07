//! Вход через Google. Здесь проверяется, кто пришёл.
//!
//! Библиотека взята намеренно, хотя всё остальное в проекте написано руками:
//! подпись S3-запроса — механика со своим входом, а разбор JWT — разбор
//! данных, которыми управляет атакующий. Там целый класс классических дыр:
//! подмена алгоритма на `none`, забытая проверка `aud`, неучтённая ротация
//! ключей. Самописная проверка выглядит работающей ровно до того дня, когда
//! перестаёт.

use anyhow::{bail, Context, Result};
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;

const JWKS_URL: &str = "https://www.googleapis.com/oauth2/v3/certs";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const AUTH_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kid: String,
    n: String,
    e: String,
}

#[derive(Debug, Deserialize)]
pub struct Identity {
    pub email: String,
    #[serde(default)]
    pub email_verified: bool,
    /// Домен Workspace. У личного Gmail отсутствует — и это единственный
    /// признак, который нельзя подделать: параметр `hd` в ссылке лишь
    /// подсказка интерфейсу, её правят руками в адресной строке.
    #[serde(default)]
    pub hd: Option<String>,
}

/// Ссылка, куда отправляем человека. `state` привязан к device-коду и
/// защищает от подстановки чужого ответа.
///
/// Параметр `hd` НЕ передаётся намеренно. Он был бы лишь подсказкой Google,
/// какой домен показывать, — обходится правкой адресной строки и потому
/// ничего не охраняет. Зато он показывал бы человеку чужой домен: сотрудник
/// одной компании видел бы в ссылке имя другой. Домен проверяется claim `hd`
/// внутри подписанного токена, и только там.
pub fn auth_url(client_id: &str, redirect_uri: &str, state: &str) -> String {
    let enc = urlencoding::encode;
    format!(
        "{AUTH_URL}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&prompt=select_account",
        enc(client_id),
        enc(redirect_uri),
        enc("openid email"),
        enc(state),
    )
}

/// Меняет одноразовый код на токены. Идёт сервер-серверу: `client_secret` в
/// браузере не появляется никогда.
pub async fn exchange_code(
    client_id: &str,
    client_secret: &str,
    redirect_uri: &str,
    code: &str,
) -> Result<String> {
    #[derive(Deserialize)]
    struct TokenResponse {
        id_token: Option<String>,
        error: Option<String>,
        error_description: Option<String>,
    }

    let resp: TokenResponse = reqwest::Client::new()
        .post(TOKEN_URL)
        .form(&[
            ("code", code),
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("redirect_uri", redirect_uri),
            ("grant_type", "authorization_code"),
        ])
        .send()
        .await
        .context("Google не ответил на обмен кода")?
        .json()
        .await
        .context("ответ Google не разобрался")?;

    if let Some(e) = resp.error {
        bail!("Google отказал: {e}{}", resp.error_description.map(|d| format!(" — {d}")).unwrap_or_default());
    }
    resp.id_token.context("Google не вернул id_token")
}

/// Проверяет `id_token` и отдаёт личность.
///
/// JWKS запрашивается на каждый вход, а не кэшируется: самозапись случается
/// редко, зато ротация ключей Google не может застать нас со старым набором.
/// Кэш здесь экономил бы миллисекунды ценой класса ошибок «вчера работало».
pub async fn verify_id_token(
    id_token: &str,
    client_id: &str,
    allowed_domains: &[String],
) -> Result<Identity> {
    let header = decode_header(id_token).context("заголовок токена не разобрался")?;
    if header.alg != Algorithm::RS256 {
        bail!("неожиданный алгоритм подписи: {:?}", header.alg);
    }
    let kid = header.kid.context("в токене нет kid")?;

    let jwks: Jwks = reqwest::get(JWKS_URL).await?.json().await.context("JWKS не разобрался")?;
    let jwk = jwks
        .keys
        .iter()
        .find(|k| k.kid == kid)
        .context("ключ подписи не найден среди ключей Google")?;

    let mut v = Validation::new(Algorithm::RS256);
    v.set_audience(&[client_id]);
    v.set_issuer(&["https://accounts.google.com", "accounts.google.com"]);
    // exp проверяется по умолчанию; оставляем это явным напоминанием.
    v.validate_exp = true;

    let data = decode::<Identity>(id_token, &DecodingKey::from_rsa_components(&jwk.n, &jwk.e)?, &v)
        .context("токен не прошёл проверку подписи или полей")?;
    let ident = data.claims;

    if !ident.email_verified {
        bail!("адрес {} не подтверждён у Google", ident.email);
    }
    match ident.hd.as_deref() {
        Some(hd) if allowed_domains.iter().any(|d| d == hd) => {}
        Some(hd) => bail!("домен {hd} не в списке разрешённых"),
        None => bail!("это личный аккаунт Google, а не рабочий: входить надо аккаунтом организации"),
    }
    Ok(ident)
}
