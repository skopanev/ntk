//! Вход без передачи ключа из рук в руки.
//!
//! Расширение показывает короткий код, человек открывает ссылку и входит
//! через Google, расширение забирает ключ само. Ключ не появляется ни в
//! переписке, ни у владельца: раздавать его руками — работа, которая никогда
//! не кончается.
//!
//! Код короткий, потому что его читают с экрана. Короткий код угадываем, и
//! защищает не он, а секрет устройства: забрать ключ может только тот, кто
//! начинал вход. Плюс срок жизни в пять минут и одна попытка забора.

use anyhow::{bail, Result};
use rand::Rng;
use sha2::{Digest, Sha256};

/// Без похожих на вид символов: 0/O и 1/I/L человек путает, диктуя код.
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
    // Секрет устройства длинный: его читает не человек, а программа.
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

/// Приводит введённый человеком код к каноническому виду: он диктуется вслух
/// и набирается как придётся — с пробелами, в нижнем регистре, без дефиса.
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

/// Ключ, который выдаётся человеку после успешного входа.
pub fn new_api_key() -> String {
    let mut rng = rand::thread_rng();
    const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let body: String = (0..40).map(|_| ALPHA[rng.gen_range(0..ALPHA.len())] as char).collect();
    format!("ntk_{body}")
}

/// Выбирает правило записи: точное совпадение по адресу побеждает домен.
/// Так личный адрес или особая роль добавляется одной строкой, без расширения
/// домена на всех.
pub fn pick_rule<'a, T>(email: &str, by_email: Option<&'a T>, by_domain: Option<&'a T>) -> Result<&'a T> {
    if let Some(r) = by_email {
        return Ok(r);
    }
    if let Some(r) = by_domain {
        return Ok(r);
    }
    bail!("для {email} нет правила доступа: ни по адресу, ни по домену")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_is_readable_aloud() {
        let s = generate();
        assert_eq!(s.code.len(), 9, "формат XXXX-XXXX: {}", s.code);
        assert_eq!(s.code.as_bytes()[4], b'-');
        for c in s.code.chars().filter(|c| *c != '-') {
            assert!(
                !"O0I1L".contains(c),
                "символ {c} путается при диктовке: {}",
                s.code
            );
        }
        assert_eq!(s.device_secret.len(), 48);
    }

    #[test]
    fn humans_type_the_code_however_they_like() {
        for typed in ["hxtp-9f2k", "HXTP 9F2K", "hxtp9f2k", " HXTP-9F2K "] {
            assert_eq!(normalize(typed), "HXTP-9F2K", "не привёлся: {typed}");
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
        assert_eq!(pick_rule("a@b.c", Some(&"по адресу"), Some(&"по домену")).unwrap(), &"по адресу");
        assert_eq!(pick_rule("a@b.c", None, Some(&"по домену")).unwrap(), &"по домену");
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
