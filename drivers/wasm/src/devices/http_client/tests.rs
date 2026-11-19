use super::{
    request::{is_forwardable, prepare, request_length},
    response::{MAX_HEAD_SIZE, build_head},
};
use alloc::{string::String, vec, vec::Vec};
use file_system::Error;
use shared::HttpResponseParser;
use wasm_bindgen_test::wasm_bindgen_test;

/// The request `Weather` writes to `/devices/network/https_client`.
const WEATHER_REQUEST: &[u8] = b"GET /v1/search?name=Paris&count=5 HTTP/1.1\r\nHost: geocoding-api.open-meteo.com\r\nConnection: close\r\nAccept: application/json\r\nAccept-Encoding: identity\r\nUser-Agent: Xila-Weather\r\n\r\n";

fn headers(prepared: &super::request::PreparedRequest<'_>) -> Vec<(String, String)> {
    prepared
        .headers
        .iter()
        .map(|(name, value)| (String::from(*name), String::from(*value)))
        .collect()
}

#[wasm_bindgen_test]
fn weather_request_becomes_a_simple_cross_origin_get() {
    let prepared = prepare("https", WEATHER_REQUEST).unwrap();
    assert_eq!(prepared.method, "GET");
    assert_eq!(
        prepared.url,
        "https://geocoding-api.open-meteo.com/v1/search?name=Paris&count=5"
    );
    // Only `Accept` survives: no `User-Agent`, `Connection`, `Host` or
    // `Accept-Encoding`, so the browser sends no preflight.
    assert_eq!(
        headers(&prepared),
        [(String::from("Accept"), String::from("application/json"))]
    );
    assert!(prepared.body.is_none());
}

#[wasm_bindgen_test]
fn forbidden_and_preflight_headers_are_dropped_in_any_case() {
    for name in [
        "Host",
        "host",
        "CONNECTION",
        "User-Agent",
        "user-agent",
        "Content-Length",
        "Accept-Encoding",
        "Transfer-Encoding",
        "Upgrade",
        "Keep-Alive",
        "TE",
        "Cookie",
        "Origin",
        "Referer",
        "Via",
        "Date",
        "Expect",
        "Proxy-Authorization",
        "Sec-Fetch-Mode",
        "Access-Control-Request-Method",
    ] {
        assert!(!is_forwardable(name), "{name} must not reach fetch");
    }
    for name in [
        "Accept",
        "Accept-Language",
        "Content-Type",
        "Authorization",
        "Range",
        "X-Api-Key",
    ] {
        assert!(
            is_forwardable(name),
            "{name} is the guest's explicit choice"
        );
    }
}

#[wasm_bindgen_test]
fn post_keeps_the_exact_body_and_content_type() {
    let request = b"POST /api HTTP/1.1\r\nhost: example.com:8443\r\ncontent-length: 4\r\nContent-Type: application/json\r\n\r\n\"a\0\0";
    let prepared = prepare("https", request).unwrap();
    assert_eq!(prepared.url, "https://example.com:8443/api");
    assert_eq!(prepared.body, Some(&b"\"a\0\0"[..]));
    assert_eq!(
        headers(&prepared),
        [(
            String::from("Content-Type"),
            String::from("application/json")
        )]
    );
}

#[wasm_bindgen_test]
fn zero_padding_written_by_the_shell_is_not_a_body() {
    // `web_request` writes its whole 4096-byte buffer.
    let mut buffer = vec![0; 4096];
    let request = b"GET /get HTTP/1.1\r\nHost: httpbin.org\r\nConnection: close\r\n\r\n";
    buffer[..request.len()].copy_from_slice(request);
    assert_eq!(request_length(&buffer), Ok(request.len()));
    let prepared = prepare("https", &buffer).unwrap();
    assert_eq!(prepared.url, "https://httpbin.org/get");
    assert!(prepared.body.is_none());

    let post = b"POST /post HTTP/1.1\r\nHost: httpbin.org\r\n\r\nhello";
    let mut buffer = vec![0; 4096];
    buffer[..post.len()].copy_from_slice(post);
    assert_eq!(prepare("https", &buffer).unwrap().body, Some(&b"hello"[..]));
}

#[wasm_bindgen_test]
fn the_device_scheme_decides_the_url_scheme() {
    let request = b"GET /x HTTP/1.1\r\nHost: localhost:8080\r\n\r\n";
    assert_eq!(
        prepare("http", request).unwrap().url,
        "http://localhost:8080/x"
    );
    let ipv6_request = b"GET /x HTTP/1.1\r\nHost: [::1]:8080\r\n\r\n";
    assert_eq!(
        prepare("https", ipv6_request).unwrap().url,
        "https://[::1]:8080/x"
    );
}

#[wasm_bindgen_test]
fn malformed_or_unsafe_requests_are_invalid_parameters() {
    for request in [
        &b"GET /x HTTP/1.1\r\nAccept: */*\r\n\r\n"[..], // no Host
        b"GET /x HTTP/1.1\r\nHost: a.example/evil?\r\n\r\n",
        b"GET /x HTTP/1.1\r\nHost: user@a.example\r\n\r\n",
        b"GET /x HTTP/1.1\r\nHost: example.com:99999\r\n\r\n",
        b"GET /x HTTP/1.1\r\nHost: example.com:abc\r\n\r\n",
        b"GET /x HTTP/1.1\r\nHost: example.com:\r\n\r\n",
        b"GET /x HTTP/1.1\r\nHost: [::1]:99999\r\n\r\n",
        b"GET /x HTTP/1.1\r\nHost: [::1\r\n\r\n",
        b"GET http://evil.example/ HTTP/1.1\r\nHost: a.example\r\n\r\n",
        b"GET x HTTP/1.1\r\nHost: a.example\r\n\r\n",
        b"CONNECT /x HTTP/1.1\r\nHost: a.example\r\n\r\n",
        b"trace /x HTTP/1.1\r\nHost: a.example\r\n\r\n",
        b"GET /x HTTP/1.1\r\nHost: a.example\r\n\r\nbody",
        b"POST /x HTTP/1.1\r\nHost: a.example\r\nContent-Length: 99\r\n\r\nshort",
        &[0; 64],
        b"",
    ] {
        assert_eq!(
            prepare("https", request).err(),
            Some(Error::InvalidParameter),
            "{:?}",
            String::from_utf8_lossy(request)
        );
    }
}

fn pairs(items: &[(&str, &str)]) -> Vec<(String, String)> {
    items
        .iter()
        .map(|(name, value)| (String::from(*name), String::from(*value)))
        .collect()
}

#[wasm_bindgen_test]
fn head_is_parsable_by_the_guest_helpers() {
    let head = build_head(
        200,
        "OK",
        &pairs(&[
            ("content-type", "application/json"),
            ("content-length", "11"),
        ]),
        false,
    )
    .unwrap();
    assert_eq!(
        head,
        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n"
    );
    let parser = HttpResponseParser::from_buffer(&head);
    assert_eq!(parser.get_status_code(), Some(200));
    assert_eq!(parser.get_body(), Some(&b""[..]));
    // HTTP/2 responses carry no reason phrase.
    let head = build_head(404, "", &[], false).unwrap();
    assert_eq!(head, b"HTTP/1.1 404\r\n\r\n");
    assert_eq!(
        HttpResponseParser::from_buffer(&head).get_status_code(),
        Some(404)
    );
}

#[wasm_bindgen_test]
fn head_drops_framing_headers_that_describe_the_encoded_body() {
    // The browser already decoded the body; keeping these would misframe it.
    let head = build_head(
        200,
        "OK",
        &pairs(&[
            ("Content-Encoding", "gzip"),
            ("Content-Length", "20"),
            ("Transfer-Encoding", "chunked"),
            ("X-Kept", "yes"),
        ]),
        false,
    )
    .unwrap();
    assert_eq!(head, b"HTTP/1.1 200 OK\r\nX-Kept: yes\r\n\r\n");
    // Under CORS, Content-Encoding is hidden while Content-Length stays visible
    // with the encoded wire length, so Content-Length is still dropped.
    let head = build_head(200, "OK", &pairs(&[("Content-Length", "674")]), false).unwrap();
    assert_eq!(head, b"HTTP/1.1 200 OK\r\n\r\n");
    // HEAD and null-body statuses carry no body, so Content-Length stays.
    let head = build_head(200, "OK", &pairs(&[("Content-Length", "674")]), true).unwrap();
    assert_eq!(head, b"HTTP/1.1 200 OK\r\nContent-Length: 674\r\n\r\n");
    for status in [100, 204, 304] {
        let head = build_head(status, "", &pairs(&[("Content-Length", "674")]), false).unwrap();
        assert_eq!(
            head,
            alloc::format!("HTTP/1.1 {status}\r\nContent-Length: 674\r\n\r\n").into_bytes()
        );
    }
}

#[wasm_bindgen_test]
fn head_larger_than_the_native_buffer_is_refused() {
    let value = "x".repeat(MAX_HEAD_SIZE);
    assert_eq!(
        build_head(200, "OK", &pairs(&[("X-Big", &value)]), false),
        Err(Error::FileTooLarge)
    );
}
