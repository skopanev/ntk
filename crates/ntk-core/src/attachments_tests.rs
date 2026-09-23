use super::*;
use serde_json::json;

fn parse(source: serde_json::Value) -> Result<AttachArgs, serde_json::Error> {
    let mut value = json!({"workspace": "test", "id": "test-123", "filename": "file.txt"});
    value
        .as_object_mut()
        .unwrap()
        .extend(source.as_object().unwrap().clone());
    serde_json::from_value(value)
}
fn args(source: serde_json::Value) -> AttachArgs {
    parse(source).unwrap()
}

#[test]
fn text_and_binary_survive_decoding() {
    assert_eq!(
        args(json!({"content": "Привет\n"})).inline_bytes().unwrap(),
        "Привет\n".as_bytes()
    );
    assert_eq!(
        args(json!({"content_base64": "AP+A"}))
            .inline_bytes()
            .unwrap(),
        [0, 255, 128]
    );
}

#[test]
fn rejects_missing_ambiguous_empty_or_invalid_sources() {
    for source in [
        json!({}),
        json!({"content": ""}),
        json!({"content_base64": ""}),
        json!({"content_base64": "not base64!"}),
        json!({"content": "hello", "content_base64": "aA=="}),
    ] {
        assert!(args(source.clone()).inline_bytes().is_err(), "{source}");
    }
}

/// Пути на диске не принимаются ВООБЩЕ — не отклоняются позже, а не
/// существуют как аргумент. Раньше поле было объявлено и отвечало «только для
/// локального stdio»; локального клиента нет с 21.09.2026, и объявленный
/// аргумент остался обещанием, на которое модель тратит попытку.
#[test]
fn a_path_on_disk_is_not_an_argument_at_all() {
    let error = parse(json!({"path": "/etc/passwd"})).unwrap_err().to_string();
    assert!(error.contains("unknown field `path`"), "{error}");
    let error = args(json!({})).inline_bytes().unwrap_err();
    assert!(error.contains("content or content_base64"), "{error}");
}

#[test]
fn limits_apply_to_bytes_not_characters() {
    assert!(check_size(MAX_BYTES).is_ok());
    assert!(check_size(MAX_BYTES + 1).is_err());
    assert!(check_size(0).is_err());
    let input = args(json!({"content": "я".repeat(MAX_BYTES / 2 + 1)}));
    assert!(input.inline_bytes().is_err());
    let input = args(json!({"content_base64": "A".repeat(MAX_BASE64 + 1)}));
    assert!(input.inline_bytes().is_err());
}
