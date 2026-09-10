//! Signing in without passing a key from hand to hand.
//!
//! The client shows a short code, the person opens a link and signs in through
//! Google, and the client collects the key itself. The key never appears in a
//! chat or in the owner's hands: handing keys out by hand is work that never
//! ends.
//!
//! The code is short because it is read off a screen. A short code is
//! guessable, and what protects it is not the code but the device secret: only
//! whoever started the sign-in can collect the key. Plus a five-minute life and
//! a single collection attempt.

use anyhow::{bail, Result};
use rand::Rng;
use sha2::{Digest, Sha256};

/// No look-alike characters: people mix up 0/O and 1/I/L when reading a code aloud.
const ALPHABET: &[u8] = b"ABCDEFGHJKMNPQRSTUVWXYZ23456789";
pub const TTL_SECONDS: i64 = 300;

pub struct Started {
    pub code: String,
    pub device_secret: String,
}

pub fn generate() -> Started {
    let mut rng = rand::thread_rng();
    let pick = |rng: &mut rand::rngs::ThreadRng, n: usize| -> String {
        (0..n).map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char).collect()
    };
    let code = format!("{}-{}", pick(&mut rng, 4), pick(&mut rng, 4));
    // The device secret is long: a program reads it, not a person.
    let device_secret: String = (0..48)
        .map(|_| {
            const HEX: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
            HEX[rand::thread_rng().gen_range(0..HEX.len())] as char
        })
        .collect();
    Started { code, device_secret }
}

pub fn hash(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// Brings a typed code to its canonical form: it gets dictated aloud and typed
/// however it lands — with spaces, in lower case, without the dash.
pub fn normalize(input: &str) -> String {
    let cleaned: String = input
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect();
    if cleaned.len() == 8 {
        format!("{}-{}", &cleaned[..4], &cleaned[4..])
    } else {
        cleaned
    }
}

/// The key handed to a person after a successful sign-in.
pub fn new_api_key() -> String {
    let mut rng = rand::thread_rng();
    const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let body: String = (0..40).map(|_| ALPHA[rng.gen_range(0..ALPHA.len())] as char).collect();
    format!("ntk_{body}")
}

/// Picks the enrolment rule: an exact address beats the domain. That way a
/// personal address or a special role is added in one line, without widening
/// the domain rule to everyone.
pub fn pick_rule<'a, T>(email: &str, by_email: Option<&'a T>, by_domain: Option<&'a T>) -> Result<&'a T> {
    if let Some(r) = by_email {
        return Ok(r);
    }
    if let Some(r) = by_domain {
        return Ok(r);
    }
    bail!("no access rule for {email}: neither by address nor by domain")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_is_readable_aloud() {
        let s = generate();
        assert_eq!(s.code.len(), 9, "expected XXXX-XXXX: {}", s.code);
        assert_eq!(s.code.as_bytes()[4], b'-');
        for c in s.code.chars().filter(|c| *c != '-') {
            assert!(
                !"O0I1L".contains(c),
                "the character {c} is confusable when read aloud: {}",
                s.code
            );
        }
        assert_eq!(s.device_secret.len(), 48);
    }

    #[test]
    fn humans_type_the_code_however_they_like() {
        for typed in ["hxtp-9f2k", "HXTP 9F2K", "hxtp9f2k", " HXTP-9F2K "] {
            assert_eq!(normalize(typed), "HXTP-9F2K", "did not normalise: {typed}");
        }
    }

    #[test]
    fn two_codes_do_not_repeat() {
        let a = generate();
        let b = generate();
        assert_ne!(a.code, b.code);
        assert_ne!(a.device_secret, b.device_secret);
    }

    #[test]
    fn an_exact_address_beats_the_domain() {
        assert_eq!(pick_rule("a@b.c", Some(&"by address"), Some(&"by domain")).unwrap(), &"by address");
        assert_eq!(pick_rule("a@b.c", None, Some(&"by domain")).unwrap(), &"by domain");
        assert!(pick_rule("a@b.c", None::<&&str>, None).is_err());
    }

    #[test]
    fn issued_keys_are_prefixed_and_long() {
        let k = new_api_key();
        assert!(k.starts_with("ntk_"));
        assert_eq!(k.len(), 44);
        assert_ne!(k, new_api_key());
    }
}
