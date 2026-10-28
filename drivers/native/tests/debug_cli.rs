#![cfg(unix)]

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    process::Command,
    thread,
};

#[test]
fn screenshot_prints_one_json_document_on_stdout() {
    let directory = std::env::temp_dir().join(format!("xila-debug-cli-{}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let socket = directory.join("socket");
    let image = directory.join("capture.png");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut request)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&request).unwrap()["op"],
            "screenshot"
        );
        stream.write_all(b"{\"ok\":true,\"result\":{\"width\":1,\"height\":1,\"format\":\"rgba8888\",\"payload_bytes\":4}}\n\x00\x00\x00\xff").unwrap();
    });
    let output = Command::new(env!("CARGO_BIN_EXE_xila-debug"))
        .arg("--endpoint")
        .arg(&socket)
        .arg("screenshot")
        .arg(&image)
        .output()
        .unwrap();
    server.join().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let response: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("stdout must contain exactly one JSON document");
    assert_eq!(response["ok"], true);
    assert!(image.exists());
    std::fs::remove_dir_all(directory).unwrap();
}
