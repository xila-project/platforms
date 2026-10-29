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
) -> Result<bool, String> {
    let mut text = None;
    let mut timeout = 3000;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--text" if text.is_none() => {
                text = Some(args.next().ok_or("--text requires a value")?)
            }
            "--timeout-ms" => {
                timeout = args
                    .next()
                    .ok_or("--timeout-ms requires a value")?
                    .parse()
                    .map_err(|_| "invalid timeout")?
            }
            _ => return Err("wait-for expects --text TEXT [--timeout-ms 1..60000]".into()),
        }
    }
    let text = text
        .filter(|text| !text.is_empty())
        .ok_or("--text requires nonempty text")?;
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
        if let Some(found) = elements
            .iter()
            .filter_map(|element| element["text"].as_str())
            .find(|value| value.contains(&text))
        {
            println!(
                "{}",
                json!({"ok": true, "result": {"matched_text": found, "truncated": result["truncated"]}})
            );
            return Ok(true);
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
        json!({"ok": false, "error": {"code": "timeout", "message": "Text was not observed before the deadline.", "truncated": truncated}})
    );
    Ok(false)
}

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "wait deadline elapsed")
}

fn ready(stream: &UnixStream, event: libc::c_short, deadline: Instant) -> io::Result<()> {
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

fn connect(endpoint: &Path, deadline: Instant) -> io::Result<UnixStream> {
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

fn query(endpoint: &Path, deadline: Instant) -> io::Result<Value> {
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
