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
pub async fn verify_id_token(
    id_token: &str,
    client_id: &str,
    allowed_domains: &[String],
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
    match ident.hd.as_deref() {
        Some(hd) if allowed_domains.iter().any(|d| d == hd) => {}
        Some(hd) => bail!("the domain {hd} is not on the allowed list"),
        None => bail!("this is a personal Google account, not a workspace one: sign in with your organisation account"),
    }
    Ok(ident)
}
