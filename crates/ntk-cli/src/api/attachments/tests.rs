use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

type Requests = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

// HTTP contract probe only: database authorization/isolation is tested against a real API.
fn server(
    fail_upload: bool,
    fail_begin: bool,
    fail_commit: bool,
) -> (String, Requests, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let base_for_thread = base.clone();
    let requests: Requests = Default::default();
    let recorded = requests.clone();
    let thread = std::thread::spawn(move || {
        let count = if fail_begin {
            1
        } else if fail_upload {
            2
        } else {
            3
        };
        for i in 0..count {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut data = Vec::new();
            let end = loop {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                data.push(byte[0]);
                if data.ends_with(b"\r\n\r\n") {
                    break data.len();
                }
            };
            let headers = String::from_utf8(data[..end].to_vec()).unwrap();
            let len: usize = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse().unwrap())
                })
                .unwrap_or(0);
            let mut body = vec![0; len];
            stream.read_exact(&mut body).unwrap();
            recorded.lock().unwrap().push((headers, body));
            let (status, response) = match i {
                0 if fail_begin => (403, json!({"error": "workspace denied"}).to_string()),
                0 => (200, json!({"url": format!("{base_for_thread}/storage?signature=secret"), "object_key": "attachments/test/test-123/key"}).to_string()),
                1 if fail_upload => (503, "storage unavailable".into()),
                1 => (200, String::new()),
                _ if fail_commit => (400, json!({"error": "object missing"}).to_string()),
                _ => (200, json!({"ticket": "test-123", "filename": "file.bin", "size_bytes": 3}).to_string()),
            };
            write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        }
    });
    (base, requests, thread)
}

fn input() -> AttachArgs {
    serde_json::from_value(json!({
        "workspace": "test", "id": "TEST-123", "filename": "file.bin",
        "content_base64": "AP+A", "content_type": "application/octet-stream",
    }))
    .unwrap()
}

#[tokio::test]
async fn upload_bytes_then_confirm_the_server_assigned_key() {
    let (base, requests, thread) = server(false, false, false);
    let result = Client::new(&base)
        .attach("test-key", input())
        .await
        .unwrap();
    thread.join().unwrap();
    assert_eq!(result["size_bytes"], 3);
    let requests = requests.lock().unwrap();
    assert!(
        requests[0]
            .0
            .starts_with("POST /v1/tickets/test-123/attachments ")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[0].1).unwrap()["size_bytes"],
        3
    );
    assert_eq!(requests[1].1, [0, 255, 128]);
    assert!(!requests[1].0.to_lowercase().contains("authorization:"));
    assert!(
        requests[2]
            .0
            .starts_with("POST /v1/tickets/test-123/attachments/commit ")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&requests[2].1).unwrap()["object_key"],
        "attachments/test/test-123/key"
    );
}

#[tokio::test]
async fn no_confirmation_after_upload_or_authorization_failure() {
    for (fail_upload, fail_begin, expected) in [(true, false, 2), (false, true, 1)] {
        let (base, requests, thread) = server(fail_upload, fail_begin, false);
        let error = Client::new(&base)
            .attach("test-key", input())
            .await
            .unwrap_err()
            .to_string();
        thread.join().unwrap();
        assert_eq!(requests.lock().unwrap().len(), expected);
        assert!(!error.contains("signature=secret"));
    }
}

#[tokio::test]
async fn confirmation_error_is_not_reported_as_success() {
    let (base, _, thread) = server(false, false, true);
    let error = Client::new(&base)
        .attach("test-key", input())
        .await
        .unwrap_err();
    thread.join().unwrap();
    assert_eq!(error.to_string(), "object missing");
}

#[test]
fn local_file_read_is_bounded_and_requires_an_absolute_file() {
    assert!(read_file("relative.txt").is_err());
    assert!(read_file(std::env::temp_dir().to_str().unwrap()).is_err());
    let path = std::env::temp_dir().join(format!("ntk-attachment-test-{}", std::process::id()));
    std::fs::write(&path, [0, 255, 128]).unwrap();
    assert_eq!(read_file(path.to_str().unwrap()).unwrap(), [0, 255, 128]);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len((MAX_BYTES + 1) as u64)
        .unwrap();
    assert!(read_file(path.to_str().unwrap()).is_err());
    std::fs::remove_file(path).unwrap();
}
