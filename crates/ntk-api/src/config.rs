//! Конфигурация читается один раз при старте и проверяется там же.
//!
//! Проверки строгие намеренно. При переносе кредов в `/etc/ntk/api.env` в
//! домен затесалась кириллическая «щ» (`acme.щone`): сервис поднялся бы,
//! а сотрудники этого домена молча не смогли бы войти — без ошибки, без лога,
//! без единого следа. Такое обязано падать при старте.

use anyhow::{bail, Context, Result};

pub struct Config {
    pub database_url: String,
    pub port: u16,
    pub google_client_id: String,
    pub google_client_secret: String,
    /// Домены, которым разрешён вход. Сверяется с claim `hd` в токене Google.
    pub google_hd: Vec<String>,
    /// Внешний адрес сервиса. Из него собирается redirect_uri, и он обязан
    /// совпадать с записанным в Google посимвольно — расхождение в один
    /// символ даёт redirect_uri_mismatch.
    pub public_url: String,
    /// Ключи Spaces. Нужны, чтобы подписать ссылку на выпуск; сами наружу не
    /// уходят никогда — клиент получает ссылку, а не креды.
    pub spaces_key: Option<String>,
    pub spaces_secret: Option<String>,
    pub spaces_bucket: Option<String>,
    pub spaces_region: Option<String>,
    pub spaces_endpoint: Option<String>,
}

impl Config {
    pub fn redirect_uri(&self) -> String {
        format!("{}/auth/google/callback", self.public_url.trim_end_matches('/'))
    }

    /// Возврат из Google для входа Claude Desktop.
    ///
    /// Тот же адрес, что и у входа с устройства, и это не экономия: список
    /// разрешённых адресов возврата живёт в консоли Google, и завести там
    /// второй — ручной шаг, без которого вход падает с redirect_uri_mismatch.
    /// Проверено: с отдельным адресом Google отказывает.
    ///
    /// Два смысла в одном обработчике развести всё равно надо, поэтому
    /// состояние помечено префиксом (см. `STATE_PREFIX`), а не угадывается по
    /// форме. Коды устройств — восемь заглавных букв и цифр, точки в них не
    /// бывает никогда.
    pub fn oauth_redirect_uri(&self) -> String {
        self.redirect_uri()
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let database_url = req("DATABASE_URL")?;
        let port: u16 = std::env::var("PORT")
            .unwrap_or_else(|_| "8080".into())
            .parse()
            .context("PORT не число")?;

        let google_hd = parse_domains(&req("GOOGLE_HD")?)?;

        Ok(Self {
            database_url,
            port,
            google_client_id: req("GOOGLE_CLIENT_ID")?,
            google_client_secret: req("GOOGLE_CLIENT_SECRET")?,
            google_hd,
            // Обязателен, и без умолчания намеренно.
            //
            // Отсюда строятся адреса в .well-known и redirect_uri для Google:
            // Claude читает документы обнаружения и идёт ПО НИМ. Умолчание на
            // адрес-заглушку давало работающий с виду сервис, который рассылал
            // клиентов в никуда — снаружи это выглядит как «MCP сломался», а в
            // журнале ни одной ошибки. Так и случилось, когда адрес убрали из
            // репозитория, а в окружение положить забыли.
            //
            // Отказ при запуске громче: сервис не поднимется, и починка займёт
            // минуту вместо вечера догадок.
            public_url: req("PUBLIC_URL")?,
            spaces_key: std::env::var("AWS_ACCESS_KEY_ID").ok(),
            spaces_secret: std::env::var("AWS_SECRET_ACCESS_KEY").ok(),
            spaces_bucket: std::env::var("SPACES_BUCKET").ok(),
            spaces_region: std::env::var("SPACES_REGION").ok(),
            spaces_endpoint: std::env::var("SPACES_ENDPOINT").ok(),
        })
    }
}

fn req(key: &str) -> Result<String> {
    let v = std::env::var(key).with_context(|| format!("{key} не задан"))?;
    if v.trim().is_empty() {
        bail!("{key} пуст");
    }
    Ok(v)
}

/// Разбирает список доменов и отвергает всё, что не выглядит доменом.
/// Не-ASCII отсекается отдельно и с внятным сообщением: это самая вероятная
/// опечатка при вводе руками, и глазами она не видна.
fn parse_domains(raw: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for part in raw.split(',') {
        let d = part.trim();
        if d.is_empty() {
            continue;
        }
        if !d.is_ascii() {
            let bad: String = d.chars().filter(|c| !c.is_ascii()).collect();
            bail!("домен «{d}» содержит не-ASCII символы: «{bad}». Похоже на смешанную раскладку");
        }
        if !d.contains('.') || d.starts_with('.') || d.ends_with('.') {
            bail!("«{d}» не похож на домен");
        }
        if !d.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
            bail!("«{d}» содержит недопустимые символы");
        }
        out.push(d.to_ascii_lowercase());
    }
    if out.is_empty() {
        bail!("GOOGLE_HD не содержит ни одного домена");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cyrillic_in_domain_is_rejected_with_a_readable_reason() {
        // Ровно тот случай, что был в /etc/ntk/api.env.
        let err = parse_domains("example.com,acme.щone").unwrap_err().to_string();
        assert!(err.contains("не-ASCII"), "сообщение должно называть причину: {err}");
        assert!(err.contains('щ'), "сообщение должно показывать сам символ: {err}");
    }

    #[test]
    fn a_plain_list_parses() {
        assert_eq!(
            parse_domains(" example.com , acme.one ").unwrap(),
            vec!["example.com", "acme.one"]
        );
    }

    #[test]
    fn nonsense_is_refused() {
        for bad in ["", "notadomain", ".leading", "trailing.", "has space.com"] {
            assert!(parse_domains(bad).is_err(), "«{bad}» должен быть отвергнут");
        }
    }
}
