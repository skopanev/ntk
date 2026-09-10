//! The configuration is read once at startup and validated in the same place.
//!
//! The checks are deliberately strict. While moving the credentials into
//! `/etc/ntk/api.env`, a Cyrillic "щ" slipped into a domain (`acme.щone`): the
//! service would have come up and the people of that domain would silently have
//! been unable to sign in — no error, no log line, not a trace. That has to
//! fail at startup.

use anyhow::{bail, Context, Result};

pub struct Config {
    pub database_url: String,
    pub port: u16,
    pub google_client_id: String,
    pub google_client_secret: String,
    /// Domains allowed to sign in. Checked against the `hd` claim in the
    /// Google token.
    pub google_hd: Vec<String>,
    /// The public address of the service. redirect_uri is built from it, and it
    /// must match what is registered with Google character for character — one
    /// character apart gives redirect_uri_mismatch.
    pub public_url: String,
    /// Spaces keys. Needed to sign a release link; they themselves never go
    /// out — the client gets a link, not credentials.
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

    /// The Google callback for the Claude Desktop sign-in.
    ///
    /// The same address as the device sign-in, and that is not thrift: the list
    /// of allowed callback addresses lives in the Google console, and adding a
    /// second one is a manual step without which the sign-in fails with
    /// redirect_uri_mismatch. Verified: with a separate address Google refuses.
    ///
    /// Two meanings in one handler still have to be told apart, so the state
    /// carries a prefix (see `STATE_PREFIX`) rather than being guessed from its
    /// shape. Device codes are eight upper-case letters and digits; a dot never
    /// appears in them.
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
            // Required, and deliberately without a default.
            //
            // The addresses in .well-known and the redirect_uri for Google are
            // built from it: Claude reads the discovery documents and follows
            // THEM. Defaulting to a placeholder address gave a service that
            // looked healthy while sending clients into nowhere — from outside
            // it reads as "MCP is broken", with not one error in the log. That
            // is exactly what happened when the address was taken out of the
            // repository and nobody put it into the environment.
            //
            // Refusing at startup is louder: the service does not come up, and
            // the fix takes a minute instead of an evening of guessing.
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
    let v = std::env::var(key).with_context(|| format!("{key} is not set"))?;
    if v.trim().is_empty() {
        bail!("{key} is empty");
    }
    Ok(v)
}

/// Parses the domain list and refuses anything that does not look like a
/// domain. Non-ASCII is rejected separately and with a plain message: it is the
/// likeliest typo when typing by hand, and the eye does not catch it.
fn parse_domains(raw: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for part in raw.split(',') {
        let d = part.trim();
        if d.is_empty() {
            continue;
        }
        if !d.is_ascii() {
            let bad: String = d.chars().filter(|c| !c.is_ascii()).collect();
            bail!("domain {d:?} contains non-ASCII characters: {bad:?}. Looks like a mixed keyboard layout");
        }
        if !d.contains('.') || d.starts_with('.') || d.ends_with('.') {
            bail!("{d:?} does not look like a domain");
        }
        if !d.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-') {
            bail!("{d:?} contains characters that are not allowed");
        }
        out.push(d.to_ascii_lowercase());
    }
    if out.is_empty() {
        bail!("GOOGLE_HD lists no domains at all");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cyrillic_in_domain_is_rejected_with_a_readable_reason() {
        // Exactly the case that was sitting in /etc/ntk/api.env.
        let err = parse_domains("example.com,acme.щone").unwrap_err().to_string();
        assert!(err.contains("non-ASCII"), "the message must name the reason: {err}");
        assert!(err.contains('щ'), "the message must show the character itself: {err}");
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
            assert!(parse_domains(bad).is_err(), "{bad:?} must be refused");
        }
    }
}
