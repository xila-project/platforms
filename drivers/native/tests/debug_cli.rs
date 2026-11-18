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

#[test]
fn ordinary_request_and_screenshot_payload_obey_total_deadline() {
    use std::time::{Duration, Instant};
    for (name,args,response) in [
        ("header",vec!["resolution"],b"".as_slice()),
        ("payload",vec!["screenshot","/tmp/xila-timeout-never-written.png"], b"{\"ok\":true,\"result\":{\"width\":1,\"height\":1,\"format\":\"rgba8888\",\"payload_bytes\":4}}\n".as_slice()),
    ] {
        let dir=std::env::temp_dir().join(format!("xila-cli-timeout-{name}-{}",std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();let socket=dir.join("socket");let listener=UnixListener::bind(&socket).unwrap();
        listener.set_nonblocking(true).unwrap();
        let response=response.to_vec();
        let server=thread::spawn(move || {
            let deadline=Instant::now()+Duration::from_secs(1);
            let mut stream=loop {match listener.accept(){Ok((s,_))=>break s,Err(_) if Instant::now()<deadline=>thread::sleep(Duration::from_millis(10)),Err(_)=>return}};
            let mut request=String::new();BufReader::new(stream.try_clone().unwrap()).read_line(&mut request).unwrap();
            let _=stream.write_all(&response);thread::sleep(Duration::from_millis(500));
        });
        let start=Instant::now();
        let output=Command::new(env!("CARGO_BIN_EXE_xila-debug")).arg("--endpoint").arg(&socket).args(["--timeout-ms","100"]).args(args).output().unwrap();
        let elapsed=start.elapsed();server.join().unwrap();
        assert!(!output.status.success());
        let value:serde_json::Value=serde_json::from_slice(&output.stdout).expect("timeout must report JSON");
        assert_eq!(value["error"]["code"],"timeout");assert!(elapsed<Duration::from_millis(400),"elapsed {elapsed:?}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn selectors_click_pointer_and_capabilities_round_trip_protocol() {
    for (index, args, op) in [
        (0, vec!["capabilities"], "capabilities"),
        (1, vec!["pointer", "click", "12", "34"], "pointer_click"),
        (
            2,
            vec!["click", "--role", "buttonmatrix", "--button", "7"],
            "click",
        ),
        (
            3,
            vec!["find", "--role", "textbox", "--name", "Username"],
            "ui_tree",
        ),
    ] {
        let dir =
            std::env::temp_dir().join(format!("xila-cli-operation-{index}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("socket");
        let listener = UnixListener::bind(&socket).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = String::new();
            BufReader::new(stream.try_clone().unwrap())
                .read_line(&mut request)
                .unwrap();
            let request: serde_json::Value = serde_json::from_str(&request).unwrap();
            assert_eq!(request["op"], op);
            if op == "click" {
                assert_eq!(request["button"], "7");
                assert_eq!(request["role"], "buttonmatrix");
            }
            if op == "pointer_click" {
                assert_eq!(request["x"], 12);
                assert_eq!(request["y"], 34);
            }
            let response = if op == "ui_tree" {
                serde_json::json!({"elements":[{"semantic_role":"textbox","name":"Username","visible":true}],"truncated":false})
            } else {
                serde_json::json!({"status":"queued"})
            };
            writeln!(
                stream,
                "{}",
                serde_json::json!({"ok":true,"result":response})
            )
            .unwrap();
        });
        let output = Command::new(env!("CARGO_BIN_EXE_xila-debug"))
            .arg("--endpoint")
            .arg(&socket)
            .args(args)
            .output()
            .unwrap();
        server.join().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(response["ok"], true);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
