use super::*;
use serde_json::json;

fn args(source: serde_json::Value) -> AttachArgs {
    let mut value = json!({"workspace": "test", "id": "test-123", "filename": "file.txt"});
    value
        .as_object_mut()
        .unwrap()
        .extend(source.as_object().unwrap().clone());
    serde_json::from_value(value).unwrap()
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
        json!({"content": "hello", "path": "/tmp/file"}),
    ] {
        assert!(args(source.clone()).inline_bytes().is_err(), "{source}");
    }
}

#[test]
fn remote_cannot_read_server_files() {
    let error = args(json!({"path": "/etc/passwd"}))
        .inline_bytes()
        .unwrap_err();
    assert!(error.contains("local stdio MCP"));
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
