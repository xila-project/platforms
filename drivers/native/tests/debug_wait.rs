#![cfg(unix)]
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    process::Command,
    thread,
    time::{Duration, Instant},
};

#[test]
fn wait_for_text_polls_until_text_appears() {
    let dir = std::env::temp_dir().join(format!("xila-wait-match-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("socket");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        for text in ["Loading", "Settings"] {
            let Some(mut stream) = accept(&listener) else {
                return;
            };
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&request).unwrap()["op"],
                "ui_tree"
            );
            writeln!(stream, "{{\"ok\":true,\"result\":{{\"elements\":[{{\"text\":\"{text}\"}}],\"truncated\":false}}}}").unwrap();
        }
    });
    let output = Command::new(env!("CARGO_BIN_EXE_xila-debug"))
        .arg("--endpoint")
        .arg(&socket)
        .args(["wait-for", "--text", "Settings", "--timeout-ms", "2000"])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["result"]["matched_text"], "Settings");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn wait_for_text_times_out_even_if_server_stalls() {
    let dir = std::env::temp_dir().join(format!("xila-wait-stall-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("socket");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        let Some(_stream) = accept(&listener) else {
            return;
        };
        thread::sleep(Duration::from_millis(500));
    });
    let start = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_xila-debug"))
        .arg("--endpoint")
        .arg(&socket)
        .args(["wait-for", "--text", "Settings", "--timeout-ms", "150"])
        .output()
        .unwrap();
    let elapsed = start.elapsed();
    server.join().unwrap();
    assert!(!output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["error"]["code"], "timeout");
    assert!(elapsed < Duration::from_millis(450), "elapsed: {elapsed:?}");
    std::fs::remove_dir_all(dir).unwrap();
}

fn accept(listener: &UnixListener) -> Option<std::os::unix::net::UnixStream> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Some(stream),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(10))
            }
            _ => return None,
        }
    }
}

#[test]
fn wait_deadline_is_not_extended_by_trickling_response() {
    let dir = std::env::temp_dir().join(format!("xila-wait-trickle-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("socket");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        let Some(mut stream) = accept(&listener) else {
            return;
        };
        for _ in 0..20 {
            if stream.write_all(b" ").is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(30));
        }
    });
    let start = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_xila-debug"))
        .arg("--endpoint")
        .arg(&socket)
        .args(["wait-for", "--text", "Settings", "--timeout-ms", "150"])
        .output()
        .unwrap();
    let elapsed = start.elapsed();
    server.join().unwrap();
    assert!(!output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["error"]["code"], "timeout");
    assert!(elapsed < Duration::from_millis(450), "elapsed: {elapsed:?}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn wait_rejects_invalid_timeouts_before_connecting() {
    for timeout in ["0", "60001", "NaN"] {
        let output = Command::new(env!("CARGO_BIN_EXE_xila-debug"))
            .args(["wait-for", "--text", "Settings", "--timeout-ms", timeout])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("timeout"));
    }
}

#[test]
fn wait_reports_incomplete_tree_on_timeout() {
    let dir = std::env::temp_dir().join(format!("xila-wait-truncated-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("socket");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let server = thread::spawn(move || {
        let Some(mut stream) = accept(&listener) else {
            return;
        };
        let mut request = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        let _ = writeln!(
            stream,
            "{{\"ok\":true,\"result\":{{\"elements\":[],\"truncated\":true}}}}"
        );
        thread::sleep(Duration::from_millis(250));
    });
    let output = Command::new(env!("CARGO_BIN_EXE_xila-debug"))
        .arg("--endpoint")
        .arg(&socket)
        .args(["wait-for", "--text", "Settings", "--timeout-ms", "150"])
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(!output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["error"]["code"], "timeout");
    assert_eq!(result["error"]["truncated"], true);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn wait_reports_invalid_socket_path_as_transport_error() {
    let path = format!("/tmp/{}", "x".repeat(256));
    let output = Command::new(env!("CARGO_BIN_EXE_xila-debug"))
        .args([
            "--endpoint",
            &path,
            "wait-for",
            "--text",
            "Settings",
            "--timeout-ms",
            "1000",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["error"]["code"], "transport_error");
}
