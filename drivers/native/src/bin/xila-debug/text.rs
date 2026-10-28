//! Bounded text admission. Acknowledgements report queued bytes, not UI completion.
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::Path,
    time::{Duration, Instant},
};

const TIMEOUT: Duration = Duration::from_secs(30);
const RETRY_DELAY: Duration = Duration::from_millis(20);

pub(super) fn send(endpoint: &Path, text: &str) -> bool {
    let deadline = Instant::now() + TIMEOUT;
    let mut confirmed = 0;
    let mut events = 0;
    while confirmed < text.len() {
        if Instant::now() >= deadline {
            print_failure(
                "timeout",
                "Timed out waiting for input queue capacity.",
                confirmed,
                false,
            );
            return false;
        }
        let rest = &text[confirmed..];
        let value = match exchange(
            endpoint,
            rest,
            deadline.saturating_duration_since(Instant::now()),
        ) {
            Ok(value) => value,
            Err(message) => {
                print_failure("transport_failed", &message, confirmed, true);
                return false;
            }
        };
        if value.get("ok").and_then(|v| v.as_bool()) != Some(true) {
            let code = value
                .pointer("/error/code")
                .and_then(|v| v.as_str())
                .unwrap_or("invalid_response");
            if code == "queue_full" {
                std::thread::sleep(
                    RETRY_DELAY.min(deadline.saturating_duration_since(Instant::now())),
                );
                continue;
            }
            let message = value
                .pointer("/error/message")
                .and_then(|v| v.as_str())
                .unwrap_or("Invalid response.");
            print_failure(code, message, confirmed, false);
            return false;
        }
        let Some(n) = value
            .pointer("/result/queued_bytes")
            .and_then(|v| v.as_u64())
            .and_then(|n| usize::try_from(n).ok())
        else {
            print_failure(
                "invalid_response",
                "Missing queued byte count; the last request may have been queued.",
                confirmed,
                true,
            );
            return false;
        };
        if n == 0 || n > rest.len() || !rest.is_char_boundary(n) {
            print_failure(
                "invalid_response",
                "Invalid queued byte count; the last request may have been queued.",
                confirmed,
                true,
            );
            return false;
        }
        confirmed += n;
        events += rest[..n].chars().count() * 2;
    }
    println!(
        "{}",
        serde_json::json!({"ok":true,"result":{"status":"queued","queued_bytes":confirmed,"queued_events":events,"complete":true}})
    );
    true
}

fn print_failure(code: &str, message: &str, confirmed: usize, uncertain: bool) {
    println!(
        "{}",
        serde_json::json!({"ok":false,"error":{"code":code,"message":message,"confirmed_queued_bytes":confirmed,"last_request_status":if uncertain {"unknown"} else {"not_queued"}}})
    );
}

fn exchange(endpoint: &Path, text: &str, timeout: Duration) -> Result<serde_json::Value, String> {
    let mut stream = UnixStream::connect(endpoint).map_err(|error| error.to_string())?;
    stream
        .set_read_timeout(Some(timeout))
        .map_err(|error| error.to_string())?;
    stream
        .set_write_timeout(Some(timeout))
        .map_err(|error| error.to_string())?;
    let mut request = serde_json::to_vec(&serde_json::json!({"op":"text","text":text}))
        .map_err(|error| error.to_string())?;
    request.push(b'\n');
    stream
        .write_all(&request)
        .map_err(|error| error.to_string())?;
    let mut header = Vec::new();
    let mut reader = BufReader::new(stream).take(debug_protocol::MAX_RESPONSE_BYTES as u64);
    reader
        .read_until(b'\n', &mut header)
        .map_err(|error| error.to_string())?;
    if header.last() != Some(&b'\n') {
        return Err("Missing or oversized response header.".into());
    }
    serde_json::from_slice(&header).map_err(|error| error.to_string())
}
