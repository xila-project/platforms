use io_error::{ResolveError, Result};
use network::TcpSocket;
use xila_http::HttpRequestParser;

/// Keep the failure kind of a DNS lookup. An unreachable network or host is a transport failure,
/// never a missing file: `ResolveError` carries its own typed `Kind`.
pub fn map_resolve_error(error: ResolveError) -> io_error::Kind {
    error.kind()
}

/// Map an embedded-io error kind (from the TLS layer or a socket adapter) back to an I/O kind.
pub(super) fn map_http_io_error(error: impl embedded_io::Error) -> io_error::Kind {
    use embedded_io::ErrorKind;
    match error.kind() {
        ErrorKind::TimedOut => io_error::Kind::TimedOut,
        ErrorKind::ConnectionRefused => io_error::Kind::ConnectionRefused,
        ErrorKind::ConnectionReset => io_error::Kind::ConnectionReset,
        ErrorKind::ConnectionAborted => io_error::Kind::ConnectionAborted,
        ErrorKind::NotConnected => io_error::Kind::NotConnected,
        ErrorKind::BrokenPipe => io_error::Kind::BrokenPipe,
        ErrorKind::PermissionDenied => io_error::Kind::PermissionDenied,
        _ => io_error::Kind::InputOutput,
    }
}

pub fn split_host_port(host_value: &str, default_port: u16) -> (&str, u16) {
    if let Some(stripped) = host_value.strip_prefix('[')
        && let Some(end) = stripped.find(']')
    {
        let host = &stripped[..end];
        let remainder = &stripped[end + 1..];
        if let Some(port_string) = remainder.strip_prefix(':')
            && let Ok(port) = port_string.parse::<u16>()
        {
            return (host, port);
        }
        return (host, default_port);
    }

    if let Some((host, port_string)) = host_value.rsplit_once(':')
        && !host.contains(':')
        && let Ok(port) = port_string.parse::<u16>()
    {
        return (host, port);
    }

    (host_value, default_port)
}

pub fn compute_request_length(buffer: &[u8], parser: HttpRequestParser<'_>) -> Result<usize> {
    let trimmed_tail = match buffer.iter().rposition(|byte| *byte != 0) {
        Some(position) => position + 1,
        None => return Err(io_error::Kind::InvalidArgument),
    };

    let headers_end = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4);

    let Some(headers_end) = headers_end else {
        return Ok(trimmed_tail);
    };

    let content_length = parser
        .get_headers()
        .find(|(name, _)| name.eq_ignore_ascii_case(HttpRequestParser::CONTENT_LENGTH_HEADER))
        .and_then(|(_, value)| value.parse::<usize>().ok());

    let body_length = match content_length {
        Some(length) => length,
        None => trimmed_tail.saturating_sub(headers_end),
    };

    let total_length = headers_end
        .checked_add(body_length)
        .ok_or(io_error::Kind::InvalidArgument)?;

    if total_length == 0 || total_length > buffer.len() {
        return Err(io_error::Kind::InvalidArgument);
    }

    Ok(total_length)
}

pub(super) struct TcpSocketAdapter {
    pub socket: TcpSocket,
}

impl embedded_io::ErrorType for TcpSocketAdapter {
    type Error = io_error::Kind;
}

impl embedded_io_async::Read for TcpSocketAdapter {
    async fn read(&mut self, buffer: &mut [u8]) -> core::result::Result<usize, io_error::Kind> {
        self.socket.read(buffer).await
    }
}

impl embedded_io_async::Write for TcpSocketAdapter {
    async fn write(&mut self, buffer: &[u8]) -> core::result::Result<usize, io_error::Kind> {
        self.socket.write(buffer).await
    }

    async fn flush(&mut self) -> core::result::Result<(), Self::Error> {
        self.socket.flush().await
    }
}

pub(super) async fn create_tcp_connection(
    manager: &'static network::Manager,
    host: &str,
    port: u16,
) -> Result<TcpSocketAdapter> {
    let addresses = manager
        .resolve(
            host,
            network::DnsQueryKind::A | network::DnsQueryKind::Aaaa,
            true,
            None,
        )
        .await
        .map_err(map_resolve_error)?;
    // A DNS response with no usable addresses is a connection failure.
    let mut last_error = io_error::Kind::HostUnreachable;
    for address in addresses {
        let mut socket = manager.new_tcp_socket(4096, 4096, None).await?;
        socket
            .set_timeout(Some(network::Duration::from_seconds(
                super::http_exchange::IO_TIMEOUT_SECONDS,
            )))
            .await;
        match embassy_futures::select::select(
            socket.connect(address, network::Port::from_inner(port)),
            task::sleep(core::time::Duration::from_secs(
                super::http_exchange::IO_TIMEOUT_SECONDS,
            )),
        )
        .await
        {
            embassy_futures::select::Either::First(Ok(())) => {
                return Ok(TcpSocketAdapter { socket });
            }
            embassy_futures::select::Either::First(Err(e)) => last_error = e,
            embassy_futures::select::Either::Second(_) => last_error = io_error::Kind::TimedOut,
        }
        socket.close_forced().await;
    }
    Err(last_error)
}

pub(super) async fn send_request<W: embedded_io_async::Write>(
    io: &mut W,
    request: &[u8],
) -> Result<()> {
    use embedded_io_async::Write;
    let parser = HttpRequestParser::from_buffer(request);
    let length = compute_request_length(request, parser)?;
    let payload = &request[..length];
    Write::write_all(io, payload)
        .await
        .map_err(map_http_io_error)?;
    if !payload.windows(4).any(|w| w == b"\r\n\r\n") {
        let suffix = if payload.ends_with(b"\r\n") {
            b"\r\n".as_slice()
        } else {
            b"\r\n\r\n".as_slice()
        };
        Write::write_all(io, suffix)
            .await
            .map_err(map_http_io_error)?;
    }
    io.flush().await.map_err(map_http_io_error)
}

impl Drop for TcpSocketAdapter {
    fn drop(&mut self) {
        // HTTP does not reuse a connection. Abort on cancellation or after the
        // framed response; the network runner sends RST before reclamation.
        task::block_on(self.socket.close_forced());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolver_failures_keep_their_transport_meaning() {
        for (error, expected) in [
            (ResolveError::NoInterface, io_error::Kind::DeviceUnavailable),
            (ResolveError::NetworkDown, io_error::Kind::NetworkDown),
            (
                ResolveError::NetworkUnreachable,
                io_error::Kind::NetworkUnreachable,
            ),
            (ResolveError::TimedOut, io_error::Kind::TimedOut),
            (ResolveError::NameTooLong, io_error::Kind::NameTooLong),
            (ResolveError::InvalidName, io_error::Kind::InvalidDnsName),
            (ResolveError::NoAnswer, io_error::Kind::NameNotResolved),
        ] {
            let kind = map_resolve_error(error);
            assert_eq!(kind, expected, "{error:?}");
            assert_ne!(kind, io_error::Kind::NotFound, "{error:?}");
        }
    }

    #[test]
    fn transport_kinds_survive_the_embedded_io_adapter() {
        assert_eq!(
            map_http_io_error(io_error::Kind::TimedOut),
            io_error::Kind::TimedOut
        );
        assert_eq!(
            map_http_io_error(io_error::Kind::ConnectionRefused),
            io_error::Kind::ConnectionRefused
        );
        assert_eq!(
            map_http_io_error(io_error::Kind::ConnectionReset),
            io_error::Kind::ConnectionReset
        );
        assert_eq!(
            map_http_io_error(embedded_tls::TlsError::Io(embedded_io::ErrorKind::TimedOut)),
            io_error::Kind::TimedOut
        );
        assert_eq!(
            map_http_io_error(embedded_tls::TlsError::IoError),
            io_error::Kind::InputOutput
        );
    }

    #[test]
    fn lowercase_length_preserves_binary_zero_tail() {
        let request = b"POST / HTTP/1.1\r\nhost: localhost\r\ncontent-length: 3\r\n\r\na\0\0";
        assert_eq!(
            compute_request_length(request, HttpRequestParser::from_buffer(request)),
            Ok(request.len())
        );
    }
}
