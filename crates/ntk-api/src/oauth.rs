//! Signing in through Google. This is where we check who turned up.
//!
//! A library was taken deliberately, although everything else in the project is
//! written by hand: signing an S3 request is mechanics with a known input,
//! while parsing a JWT is parsing data an attacker controls. There is a whole
//! class of classic holes there: the algorithm swapped to `none`, a forgotten
//! `aud` check, unaccounted key rotation. A hand-rolled check looks like it
//! works right up to the day it stops.

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
    /// The Workspace domain. Absent for a personal Gmail — and this is the one
    /// signal that cannot be forged: the `hd` parameter in the link is only a
    /// hint to the interface, editable by hand in the address bar.
    #[serde(default)]
    pub hd: Option<String>,
}

/// The link a person is sent to. `state` is tied to the device code and guards
/// against somebody else's answer being substituted.
///
/// The `hd` parameter is deliberately NOT passed. It would only hint to Google
/// which domain to show — bypassed by editing the address bar, and therefore
/// guarding nothing. What it would do is show a person the wrong domain: an
/// employee of one company would see another company's name in the link. The
/// domain is checked by the `hd` claim inside the signed token, and only
/// there.
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

/// Exchanges the one-time code for tokens. Server to server: `client_secret`
/// never appears in a browser.
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

/// Verifies the `id_token` and returns the identity.
///
/// JWKS is fetched on every sign-in rather than cached: sign-ins are rare,
/// whereas Google rotating its keys must never catch us holding the old set. A
/// cache here would save milliseconds at the price of a class of bugs that read
/// as "it worked yesterday".
/// Пускать ли этого человека. Отдельной функцией — потому что это решение о
/// ДОСТУПЕ, и его надо уметь проверить без Google и без сети.
fn admitted(
    hd: Option<&str>,
    email: &str,
    allowed_domains: &[String],
    named_addresses: &[String],
) -> Result<()> {
    let named = named_addresses.iter().any(|a| a.eq_ignore_ascii_case(email));
    match hd {
        Some(hd) if allowed_domains.iter().any(|d| d == hd) => Ok(()),
        Some(_) if named => Ok(()),
        Some(hd) => bail!("the domain {hd} is not on the allowed list"),
        None if named => Ok(()),
        None => bail!(
            "this is a personal Google account, not a workspace one: sign in with your organisation \
             account, or ask an administrator to add this exact address"
        ),
    }
}

pub async fn verify_id_token(
    id_token: &str,
    client_id: &str,
    allowed_domains: &[String],
    named_addresses: &[String],
) -> Result<Identity> {
    let header = decode_header(id_token).context("the token header did not parse")?;
    if header.alg != Algorithm::RS256 {
        bail!("unexpected signature algorithm: {:?}", header.alg);
    }
    let kid = header.kid.context("the token carries no kid")?;

    let jwks: Jwks = reqwest::get(JWKS_URL).await?.json().await.context("JWKS не разобрался")?;
    let jwk = jwks
        .keys
        .iter()
        .find(|k| k.kid == kid)
        .context("the signing key is not among Google's keys")?;

    let mut v = Validation::new(Algorithm::RS256);
    v.set_audience(&[client_id]);
    v.set_issuer(&["https://accounts.google.com", "accounts.google.com"]);
    // exp is validated by default; kept here as an explicit reminder.
    v.validate_exp = true;

    let data = decode::<Identity>(id_token, &DecodingKey::from_rsa_components(&jwk.n, &jwk.e)?, &v)
        .context("the token failed signature or claim validation")?;
    let ident = data.claims;

    if !ident.email_verified {
        bail!("the address {} is not verified with Google", ident.email);
    }
    // Личный адрес пускается, если администратор назвал ИМЕННО ЕГО.
    //
    // Здесь стояло глухое «личный аккаунт — не пущу», и оно спорило с
    // соседним слоем: разбор правил зачисления прямо обещает, что «личный
    // адрес добавляется одной строкой, без расширения доменного правила на
    // всех». Обещание в одном месте, запрет в другом — и до правила дело не
    // доходило вовсе, потому что вход отвергался раньше.
    //
    // Заслон при этом остаётся закрытым: пускается не «любой gmail», а ровно
    // тот адрес, который выписан в core.enrollment_rules отдельной строкой.
    // Доменное правило такой силы НЕ даёт — иначе один публичный почтовик
    // открыл бы воркспейс всему свету.
    admitted(ident.hd.as_deref(), &ident.email, allowed_domains, named_addresses)?;
    Ok(ident)
}

#[cfg(test)]
mod admission_tests {
    use super::admitted;

    fn v(xs: &[&str]) -> Vec<String> {
        xs.iter().map(|s| s.to_string()).collect()
    }

    /// Корпоративный аккаунт с разрешённого домена — как было.
    #[test]
    fn an_allowed_domain_passes() {
        assert!(admitted(Some("acme.example"), "a@acme.example", &v(&["acme.example"]), &[]).is_ok());
    }

    /// Чужой домен не пускается, сколько бы личных адресов ни было выписано.
    #[test]
    fn an_unlisted_domain_is_refused() {
        let e = admitted(Some("other.example"), "a@other.example", &v(&["acme.example"]), &v(&["b@mail.example"]));
        assert!(e.is_err());
    }

    /// Личный аккаунт БЕЗ поимённой записи — отказ, как и был.
    ///
    /// Это половина, ради которой заслон и стоит: без неё один публичный
    /// почтовик открыл бы воркспейс всему свету.
    #[test]
    fn a_personal_account_is_still_refused_by_default() {
        let e = admitted(None, "stranger@mail.example", &v(&["acme.example"]), &v(&["named@mail.example"]));
        assert!(e.is_err(), "личный адрес без записи обязан быть отвергнут");
        assert!(format!("{}", e.unwrap_err()).contains("personal Google account"));
    }

    /// Личный аккаунт, выписанный ПОИМЁННО, — пускается.
    #[test]
    fn a_named_personal_address_passes() {
        assert!(admitted(None, "named@mail.example", &[], &v(&["named@mail.example"])).is_ok());
    }

    /// Регистр в адресе значения не имеет: человек наберёт как придётся.
    #[test]
    fn the_address_matches_regardless_of_case() {
        assert!(admitted(None, "Named@Mail.Example", &[], &v(&["named@mail.example"])).is_ok());
    }

    /// Пустой список поимённых закрывает заслон, а не открывает.
    ///
    /// Список читается из базы на каждый вход, и отказ базы отдаёт пустоту.
    /// Пустота обязана означать «никого дополнительно», а не «всех».
    #[test]
    fn an_empty_list_admits_nobody_extra() {
        assert!(admitted(None, "anyone@mail.example", &v(&["acme.example"]), &[]).is_err());
    }
}
