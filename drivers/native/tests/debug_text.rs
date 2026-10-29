#![cfg(unix)]
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    process::Command,
    thread,
};

#[test]
fn text_retries_queue_full_and_unicode_prefixes() {
    let dir = std::env::temp_dir().join(format!("xila-debug-text-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("control.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        for (expected, response) in [
            (
                "é中",
                r#"{"ok":false,"error":{"code":"queue_full","message":"full"}}"#,
            ),
            (
                "é中",
                r#"{"ok":true,"result":{"queued_bytes":2,"complete":false}}"#,
            ),
            (
                "中",
                r#"{"ok":true,"result":{"queued_bytes":3,"complete":true}}"#,
            ),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let request: serde_json::Value = serde_json::from_str(&request).unwrap();
            assert_eq!(request["text"], expected);
            writeln!(stream, "{response}").unwrap();
        }
    });
    let output = Command::new(env!("CARGO_BIN_EXE_xila-debug"))
        .arg("--endpoint")
        .arg(&socket)
        .args(["text", "é中"])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["result"]["queued_bytes"], 5);
    assert_eq!(result["result"]["queued_events"], 4);
    assert_eq!(result["result"]["status"], "queued");
    assert_eq!(result["result"]["unicode_window_events"], "unsupported");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn disconnected_response_reports_uncertain_last_request() {
    let dir = std::env::temp_dir().join(format!("xila-debug-disconnect-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("control.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(stream).read_line(&mut request).unwrap();
    });
    let output = Command::new(env!("CARGO_BIN_EXE_xila-debug"))
        .arg("--endpoint")
        .arg(&socket)
        .args(["text", "abc"])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(!output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["error"]["confirmed_queued_bytes"], 0);
    assert_eq!(result["error"]["last_request_status"], "unknown");
    std::fs::remove_dir_all(dir).unwrap();
}
