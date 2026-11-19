use file_system::{Error, Result};
use shared::HttpRequestParser;

pub fn map_network_error(error: network::Error) -> Error {
    match error {
        network::Error::NotFound => Error::NotFound,
        network::Error::PermissionDenied => Error::PermissionDenied,
        network::Error::ConnectionRefused
        | network::Error::ConnectionReset
        | network::Error::ConnectionAborted
        | network::Error::NotConnected
        | network::Error::BrokenPipe
        | network::Error::NoRoute
        | network::Error::HostUnreachable
        | network::Error::NetworkUnreachable
        | network::Error::NetworkDown => Error::InputOutput,
        // The resolver reports Failed for unsuccessful queries without a more specific cause.
        network::Error::Failed => Error::InputOutput,
        network::Error::TimedOut => Error::TimeError,
        network::Error::InvalidInput
        | network::Error::InvalidData
        | network::Error::InvalidPort
        | network::Error::InvalidEndpoint
        | network::Error::InvalidName => Error::InvalidParameter,
        network::Error::NameTooLong => Error::NameTooLong,
        network::Error::ResourceBusy | network::Error::Pending | network::Error::NoFreeSlot => {
            Error::RessourceBusy
        }
        _ => Error::Other,
    }
}

pub(super) fn map_http_io_error(error: impl embedded_io::Error) -> Error {
    match error.kind() {
        embedded_io::ErrorKind::TimedOut => Error::TimeError,
        _ => Error::InputOutput,
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
        None => return Err(Error::InvalidParameter),
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
        .ok_or(Error::InvalidParameter)?;

    if total_length == 0 || total_length > buffer.len() {
        return Err(Error::InvalidParameter);
    }

    Ok(total_length)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_errors_keep_their_closest_file_system_meaning() {
        use network::Error as Net;

        for (network, expected) in [
            (Net::NotFound, Error::NotFound),
            (Net::PermissionDenied, Error::PermissionDenied),
            (Net::NoRoute, Error::InputOutput),
            (Net::HostUnreachable, Error::InputOutput),
            (Net::NetworkUnreachable, Error::InputOutput),
            (Net::NetworkDown, Error::InputOutput),
            (Net::ConnectionRefused, Error::InputOutput),
            (Net::ConnectionReset, Error::InputOutput),
            (Net::ConnectionAborted, Error::InputOutput),
            (Net::TimedOut, Error::TimeError),
            (Net::Failed, Error::InputOutput),
            (Net::InvalidName, Error::InvalidParameter),
            (Net::NameTooLong, Error::NameTooLong),
            (Net::NoFreeSlot, Error::RessourceBusy),
            (Net::Pending, Error::RessourceBusy),
        ] {
            assert_eq!(map_network_error(network), expected, "{network:?}");
        }
    }

    #[test]
    fn transport_timeout_survives_the_embedded_io_adapter() {
        let socket_error = IoError::FileSystem(map_network_error(network::Error::TimedOut));
        assert_eq!(map_http_io_error(socket_error), Error::TimeError);
        assert_eq!(
            map_http_io_error(embedded_tls::TlsError::Io(embedded_io::ErrorKind::TimedOut)),
            Error::TimeError
        );
        assert_eq!(
            map_http_io_error(embedded_tls::TlsError::IoError),
            Error::InputOutput
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

use network::TcpSocket;
#[derive(Debug, Copy, Clone)]
pub(super) enum IoError {
    FileSystem(Error),
}

impl embedded_io::Error for IoError {
    fn kind(&self) -> embedded_io::ErrorKind {
        match self {
            IoError::FileSystem(Error::NotFound) => embedded_io::ErrorKind::NotFound,
            IoError::FileSystem(Error::TimeError) => embedded_io::ErrorKind::TimedOut,
            IoError::FileSystem(Error::PermissionDenied) => {
                embedded_io::ErrorKind::PermissionDenied
            }
            _ => embedded_io::ErrorKind::Other,
        }
    }
}

impl core::fmt::Display for IoError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            IoError::FileSystem(error) => write!(formatter, "filesystem error: {:?}", error),
        }
    }
}

impl core::error::Error for IoError {}

pub(super) struct TcpSocketAdapter {
    pub socket: TcpSocket,
}

impl embedded_io::ErrorType for TcpSocketAdapter {
    type Error = IoError;
}

impl embedded_io_async::Read for TcpSocketAdapter {
    async fn read(&mut self, buffer: &mut [u8]) -> core::result::Result<usize, Self::Error> {
        self.socket
            .read(buffer)
            .await
            .map_err(map_network_error)
            .map_err(IoError::FileSystem)
    }
}

impl embedded_io_async::Write for TcpSocketAdapter {
    async fn write(&mut self, buffer: &[u8]) -> core::result::Result<usize, Self::Error> {
        self.socket
            .write(buffer)
            .await
            .map_err(map_network_error)
            .map_err(IoError::FileSystem)
    }

    async fn flush(&mut self) -> core::result::Result<(), Self::Error> {
        self.socket
            .flush()
            .await
            .map_err(map_network_error)
            .map_err(IoError::FileSystem)
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
        .map_err(map_network_error)?;
    // A DNS response with no usable addresses is a connection failure.
    let mut last_error = Error::InputOutput;
    for address in addresses {
        let mut socket = manager
            .new_tcp_socket(4096, 4096, None)
            .await
            .map_err(map_network_error)?;
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
            embassy_futures::select::Either::First(Err(e)) => last_error = map_network_error(e),
            embassy_futures::select::Either::Second(_) => last_error = Error::TimeError,
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
