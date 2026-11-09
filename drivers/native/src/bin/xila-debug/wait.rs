//! Host-side bounded polling of the existing UI-tree operation.
use serde_json::{Value, json};
use std::{
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, net::UnixStream},
    },
    path::Path,
    time::{Duration, Instant},
};

const POLL_INTERVAL: Duration = Duration::from_millis(100);
const MAX_TIMEOUT_MS: u64 = 60_000;

pub fn run(
    endpoint: Option<std::path::PathBuf>,
    mut args: impl Iterator<Item = String>,
    default_timeout: Option<u64>,
) -> Result<bool, String> {
    let mut selector = super::selector::Selector::default();
    let mut stable_ms = 0u64;
    let mut stable_since = None;
    let mut stable_value = None;
    let mut timeout = default_timeout.unwrap_or(3000);
    let mut timeout_seen = false;
    let mut stable_seen = false;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--stable-ms" => {
                if stable_seen {
                    return Err("duplicate --stable-ms".into());
                }
                stable_seen = true;
                stable_ms = args
                    .next()
                    .ok_or("--stable-ms requires a value")?
                    .parse()
                    .map_err(|_| "invalid stable interval")?;
            }
            "--text" | "--name" | "--role" | "--button" | "--value" | "--checked" | "--focused"
            | "--enabled" | "--layer" => {
                selector.set(&flag, args.next().ok_or("selector flag requires a value")?)?
            }
            "--timeout-ms" => {
                if timeout_seen {
                    return Err("duplicate --timeout-ms".into());
                }
                timeout_seen = true;
                timeout = args
                    .next()
                    .ok_or("--timeout-ms requires a value")?
                    .parse()
                    .map_err(|_| "invalid timeout")?
            }
            _ => {
                return Err(
                    "wait-for expects selectors [--stable-ms MS] [--timeout-ms 1..60000]".into(),
                );
            }
        }
    }
    selector.validate()?;
    if stable_ms > timeout {
        return Err("stable interval exceeds timeout".into());
    }
    if !(1..=MAX_TIMEOUT_MS).contains(&timeout) {
        return Err("timeout must be between 1 and 60000 ms".into());
    }
    let endpoint = match endpoint {
        Some(endpoint) => endpoint,
        None => drivers_native::window_screen::control::transport::endpoint_path()?,
    };
    let deadline = Instant::now() + Duration::from_millis(timeout);
    let mut saw_truncation = false;
    loop {
        let response = match query(&endpoint, deadline) {
            Ok(response) => response,
            Err(error) if error.kind() == io::ErrorKind::TimedOut => {
                return timeout_response(saw_truncation);
            }
            Err(error) => {
                println!(
                    "{}",
                    json!({"ok": false, "error": {"code": "transport_error", "message": error.to_string()}})
                );
                return Ok(false);
            }
        };
        match response.get("ok") {
            Some(Value::Bool(false)) => {
                println!("{response}");
                return Ok(false);
            }
            Some(Value::Bool(true)) => {}
            _ => return invalid_response("missing status"),
        }
        let result = &response["result"];
        let Some(elements) = result["elements"].as_array() else {
            return invalid_response("missing elements");
        };
        let Some(truncated) = result["truncated"].as_bool() else {
            return invalid_response("missing truncation status");
        };
        saw_truncation |= truncated;
        let found = elements
            .iter()
            .find(|element| selector.matches(element, true));
        if let Some(found) = found {
            if stable_ms > 0
                && elements
                    .iter()
                    .filter(|e| selector.matches(e, true))
                    .count()
                    > 1
            {
                println!(
                    "{}",
                    json!({"ok":false,"error":{"code":"ambiguous_target","message":"Stable waits require a unique matching widget."}})
                );
                return Ok(false);
            }
            let signature = stable_signature(found);
            if stable_value.as_ref() != Some(&signature) {
                stable_since = Some(Instant::now());
                stable_value = Some(signature);
            }
            if stable_since.is_some_and(|t| t.elapsed() >= Duration::from_millis(stable_ms)) {
                println!(
                    "{}",
                    json!({"ok":true,"result":{"matched_text":found["text"],"element":found,"truncated":truncated,"status":"observed"}})
                );
                return Ok(true);
            }
        } else {
            stable_since = None;
            stable_value = None;
        }
        let remaining = match deadline.checked_duration_since(Instant::now()) {
            Some(time) if !time.is_zero() => time,
            _ => return timeout_response(saw_truncation),
        };
        std::thread::sleep(remaining.min(POLL_INTERVAL));
    }
}

fn invalid_response(reason: &str) -> Result<bool, String> {
    println!(
        "{}",
        json!({"ok": false, "error": {"code": "invalid_response", "message": reason}})
    );
    Ok(false)
}

fn timeout_response(truncated: bool) -> Result<bool, String> {
    println!(
        "{}",
        json!({"ok": false, "error": {"code": "timeout", "message": "Matching widget state was not observed before the deadline.", "truncated": truncated}})
    );
    Ok(false)
}

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "wait deadline elapsed")
}

pub(super) fn ready(
    stream: &UnixStream,
    event: libc::c_short,
    deadline: Instant,
) -> io::Result<()> {
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(timed_out)?;
        if remaining.is_zero() {
            return Err(timed_out());
        }
        let millis = remaining
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        let mut fd = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: event,
            revents: 0,
        };
        // SAFETY: `fd` is a valid pollfd and the count matches the one-element buffer.
        let result = unsafe { libc::poll(&mut fd, 1, millis) };
        if result > 0 {
            return if Instant::now() < deadline {
                Ok(())
            } else {
                Err(timed_out())
            };
        }
        if result == 0 {
            return Err(timed_out());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

pub(super) fn connect(endpoint: &Path, deadline: Instant) -> io::Result<UnixStream> {
    let name = endpoint.as_os_str().as_bytes();
    // SAFETY: an all-zero sockaddr_un is valid and is populated before use.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if name.is_empty() || name.contains(&0) || name.len() >= address.sun_path.len() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid Unix socket path",
        ));
    }
    address.sun_family = libc::AF_UNIX as _;
    // Match std::os::unix::net's pathname length, including the terminating NUL.
    let address_len = std::mem::offset_of!(libc::sockaddr_un, sun_path) + name.len() + 1;
    #[cfg(any(
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    {
        address.sun_len = address_len as u8;
    }
    for (dst, src) in address.sun_path.iter_mut().zip(name) {
        *dst = *src as _;
    }
    // SAFETY: the constants specify an ordinary Unix stream socket.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: socket returned a new owned descriptor; UnixStream closes it.
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    stream.set_nonblocking(true)?;
    if Instant::now() >= deadline {
        return Err(timed_out());
    }
    // SAFETY: address is initialized, NUL-terminated, and lives for this call.
    let status = unsafe {
        libc::connect(
            stream.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            address_len as libc::socklen_t,
        )
    };
    if status < 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(error);
        }
        ready(&stream, libc::POLLOUT, deadline)?;
        if let Some(error) = stream.take_error()? {
            return Err(error);
        }
    }
    Ok(stream)
}

pub(super) fn query(endpoint: &Path, deadline: Instant) -> io::Result<Value> {
    exchange(
        endpoint,
        deadline,
        b"{\"op\":\"ui_tree\",\"max_elements\":256,\"max_depth\":24}\n",
    )
}

pub(super) fn exchange(endpoint: &Path, deadline: Instant, request: &[u8]) -> io::Result<Value> {
    let mut stream = connect(endpoint, deadline)?;
    let mut written = 0;
    while written < request.len() {
        ready(&stream, libc::POLLOUT, deadline)?;
        match stream.write(&request[written..]) {
            Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "socket closed")),
            Ok(count) => written += count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        }
    }
    let mut bytes = Vec::new();
    let mut chunk = [0; 1024];
    loop {
        ready(&stream, libc::POLLIN, deadline)?;
        let count = match stream.read(&mut chunk) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "socket closed before response",
                ));
            }
            Ok(count) => count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        };
        let end = chunk[..count].iter().position(|byte| *byte == b'\n');
        let length = end.unwrap_or(count);
        if bytes.len() + length >= debug_protocol::MAX_RESPONSE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Response exceeds limit",
            ));
        }
        bytes.extend_from_slice(&chunk[..length]);
        if end.is_some() {
            return serde_json::from_slice(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error));
        }
    }
}

fn stable_signature(element: &Value) -> Value {
    let mut signature = element.clone();
    if let Some(object) = signature.as_object_mut() {
        object.remove("id");
        object.remove("parent_id");
    }
    signature
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_observation_ignores_snapshot_indexes_but_not_state() {
        let a = json!({"id":3,"parent_id":1,"value":42});
        let b = json!({"id":9,"parent_id":5,"value":42});
        assert_eq!(stable_signature(&a), stable_signature(&b));
        assert_ne!(stable_signature(&a), stable_signature(&json!({"value":43})));
    }
}
