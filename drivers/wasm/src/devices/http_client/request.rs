//! Translation of the guest's raw HTTP/1.1 request into the parts `fetch` accepts.
//!
//! The guest speaks the same protocol as the native `http_client` and
//! `https_client` devices: one write holding the request line, headers and
//! optional body. Browsers own the connection, so only what `fetch` can express
//! survives the translation.
use alloc::{format, string::String, vec::Vec};
use file_system::{Error, Result};
use shared::HttpRequestParser;

pub(super) struct PreparedRequest<'a> {
    pub method: &'a str,
    pub url: String,
    /// Only headers a page may send without asking the server's permission first.
    pub headers: Vec<(&'a str, &'a str)>,
    pub body: Option<&'a [u8]>,
}

/// Whether `fetch` may carry this header.
///
/// Dropped headers are either forbidden to pages (the browser sets them), or
/// not CORS-safelisted, so an engine that honours them has to send a preflight
/// `OPTIONS` request that many public JSON APIs do not answer (Open-Meteo's
/// geocoding endpoint replies 404). `User-Agent` is in the second group: guests
/// send one on every native request, the browser owns that identity, and
/// engines differ in whether a page may set it (Chromium 156 accepts it without a
/// preflight; the specification does not safelist it). `Content-Type` and
/// `Authorization` are forwarded because the guest asked for them explicitly:
/// dropping either would change the request's meaning.
pub(super) fn is_forwardable(name: &str) -> bool {
    const DROPPED: &[&str] = &[
        "accept-charset",
        "accept-encoding",
        "connection",
        "content-length",
        "cookie",
        "cookie2",
        "date",
        "dnt",
        "expect",
        "host",
        "keep-alive",
        "origin",
        "referer",
        "set-cookie",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "user-agent",
        "via",
    ];
    const DROPPED_PREFIXES: &[&str] = &["proxy-", "sec-", "access-control-request-"];
    let starts_with = |prefix: &str| {
        name.len() >= prefix.len()
            && name.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
    };
    !DROPPED
        .iter()
        .any(|dropped| name.eq_ignore_ascii_case(dropped))
        && !DROPPED_PREFIXES.iter().any(|prefix| starts_with(prefix))
}

fn is_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

fn is_host(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-_:[]".contains(&byte))
}

/// Length of the request inside the buffer the guest wrote.
///
/// Mirrors the native devices: an explicit `Content-Length` wins, otherwise
/// trailing zero padding (shell commands write a fixed 4 KiB buffer) is ignored.
pub(super) fn request_length(buffer: &[u8]) -> Result<usize> {
    let trimmed_tail = buffer
        .iter()
        .rposition(|byte| *byte != 0)
        .map(|position| position + 1)
        .ok_or(Error::InvalidParameter)?;
    let Some(headers_end) = buffer
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|position| position + 4)
    else {
        return Ok(trimmed_tail);
    };
    let content_length = HttpRequestParser::from_buffer(buffer)
        .get_headers()
        .find(|(name, _)| name.eq_ignore_ascii_case(HttpRequestParser::CONTENT_LENGTH_HEADER))
        .and_then(|(_, value)| value.parse::<usize>().ok());
    let body_length = content_length.unwrap_or_else(|| trimmed_tail.saturating_sub(headers_end));
    let total = headers_end
        .checked_add(body_length)
        .ok_or(Error::InvalidParameter)?;
    if total == 0 || total > buffer.len() {
        return Err(Error::InvalidParameter);
    }
    Ok(total)
}

pub(super) fn prepare<'a>(scheme: &str, buffer: &'a [u8]) -> Result<PreparedRequest<'a>> {
    let buffer = &buffer[..request_length(buffer)?];
    let parser = HttpRequestParser::from_buffer(buffer);
    let (method, path) = parser.get_request().ok_or(Error::InvalidParameter)?;
    if !is_token(method)
        || ["CONNECT", "TRACE", "TRACK"]
            .iter()
            .any(|forbidden| method.eq_ignore_ascii_case(forbidden))
    {
        return Err(Error::InvalidParameter);
    }
    // Origin-form only: the URL's authority comes from the Host header, as on
    // the native devices, and cannot be redirected by an absolute-form target.
    if !path.starts_with('/')
        || path
            .bytes()
            .any(|byte| byte.is_ascii_control() || byte == b' ')
    {
        return Err(Error::InvalidParameter);
    }
    let host = parser
        .get_headers()
        .find(|(name, _)| name.eq_ignore_ascii_case(HttpRequestParser::HOST_HEADER))
        .map(|(_, value)| value)
        .filter(|host| is_host(host))
        .ok_or(Error::InvalidParameter)?;
    let headers = parser
        .get_headers()
        .filter(|(name, _)| is_forwardable(name))
        .collect();
    let body = parser.get_body().filter(|body| !body.is_empty());
    if body.is_some() && (method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD"))
    {
        return Err(Error::InvalidParameter);
    }
    Ok(PreparedRequest {
        method,
        url: format!("{scheme}://{host}{path}"),
        headers,
        body,
    })
}
