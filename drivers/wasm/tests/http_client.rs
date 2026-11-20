//! Device-level tests with a replaced `globalThis.fetch`. They run under
//! Node (`wasm-bindgen-test-runner`), which provides `Response`, `Headers`,
//! `ReadableStream` and `AbortController`, and need no network.
#![cfg(target_arch = "wasm32")]

use core::{
    future::poll_fn,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context as TaskContext, Poll, RawWaker, RawWakerVTable, Waker},
};
use drivers_wasm::devices::HttpClientDevice;
use file_system::{BaseOperations, Context, IoErrorKind, Operation, Result};
use js_sys::{Function, Reflect};
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use wasm_bindgen_test::*;
use xila_http::{BodyDecoder, BodyKind, parse_response_head};

drivers_wasm::memory::instantiate_global_allocator!();

const HELPERS: &str = r#"
const mock = globalThis.xilaMock = {
  calls: [], pulls: 0, handler: null,
  stream(parts, status, headers, failAfter) {
    let i = 0;
    const body = new ReadableStream({
      pull: (controller) => {
        mock.pulls++;
        if (failAfter !== undefined && i === failAfter) controller.error(new TypeError('network error'));
        else if (i < parts.length) controller.enqueue(new TextEncoder().encode(parts[i++]));
        else controller.close();
      },
    }, { highWaterMark: 0 });
    return new Response(body, { status, headers, statusText: status === 200 ? 'OK' : 'Created' });
  },
};
globalThis.fetch = (url, init) => {
  mock.calls.push({
    url, method: init.method, cache: init.cache, credentials: init.credentials, referrerPolicy: init.referrerPolicy,
    headers: [...new Headers(init.headers)].map(([n, v]) => n + ': ' + v).join('|'),
    body: init.body ? new TextDecoder().decode(init.body) : null, signal: init.signal,
  });
  return Promise.resolve().then(() => mock.handler(url, init));
};
return mock;
"#;

fn mock() -> JsValue {
    Function::new_no_args(HELPERS)
        .call0(&JsValue::NULL)
        .unwrap()
}

fn handler(mock: &JsValue, source: &str) {
    let function = Function::new_with_args("mock", &return_expression(source));
    let handler = function.call1(&JsValue::NULL, mock).unwrap();
    Reflect::set(mock, &"handler".into(), &handler).unwrap();
}

fn return_expression(source: &str) -> String {
    format!("return {source};")
}

fn field(mock: &JsValue, path: &str) -> JsValue {
    path.split('.').fold(mock.clone(), |value, key| {
        Reflect::get(&value, &key.into()).unwrap()
    })
}

fn call(mock: &JsValue, index: u32, key: &str) -> String {
    let calls = field(mock, "calls");
    let call = Reflect::get(&calls, &index.into()).unwrap();
    Reflect::get(&call, &key.into())
        .unwrap()
        .as_string()
        .unwrap_or_default()
}

fn aborted(mock: &JsValue, index: u32) -> bool {
    let calls = field(mock, "calls");
    let call = Reflect::get(&calls, &index.into()).unwrap();
    let signal = Reflect::get(&call, &"signal".into()).unwrap();
    Reflect::get(&signal, &"aborted".into())
        .unwrap()
        .as_bool()
        .unwrap()
}

fn open(device: &HttpClientDevice) -> Context {
    let mut context = Context::new_empty();
    device.open(&mut context).unwrap();
    context
}

fn write(device: &HttpClientDevice, context: &mut Context, request: &[u8]) -> Result<usize> {
    let mut cx = TaskContext::from_waker(Waker::noop());
    match device.write(context, &mut cx, request, 0) {
        Poll::Ready(result) => result,
        Poll::Pending => panic!("write must complete synchronously"),
    }
}

async fn read(
    device: &HttpClientDevice,
    context: &mut Context,
    buffer: &mut [u8],
) -> Result<usize> {
    poll_fn(|cx| device.read(context, cx, buffer, 0)).await
}

/// Everything the guest would read after the head, with a small buffer.
async fn read_all(
    device: &HttpClientDevice,
    context: &mut Context,
    size: usize,
) -> Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = vec![0; size];
    loop {
        match read(device, context, &mut buffer).await? {
            0 => return Ok(output),
            count => output.extend_from_slice(&buffer[..count]),
        }
    }
}

const GET: &[u8] = b"GET /v1/search?name=Paris HTTP/1.1\r\nHost: geocoding-api.open-meteo.com\r\nConnection: close\r\nAccept: application/json\r\nAccept-Encoding: identity\r\nUser-Agent: Xila-Weather\r\n\r\n";

#[wasm_bindgen_test]
async fn weather_style_get_streams_head_then_body_and_ends_with_zero() {
    let mock = mock();
    handler(
        &mock,
        r#"() => mock.stream(['{"results":', '[{"name":"Paris"}]}'], 200, { 'content-type': 'application/json' })"#,
    );
    let device = HttpClientDevice::HTTPS;
    let mut context = open(&device);
    assert_eq!(write(&device, &mut context, GET), Ok(GET.len()));

    // Same call as the browser fetch: simple GET, nothing that forces a preflight.
    assert_eq!(
        call(&mock, 0, "url"),
        "https://geocoding-api.open-meteo.com/v1/search?name=Paris"
    );
    assert_eq!(call(&mock, 0, "method"), "GET");
    assert_eq!(call(&mock, 0, "cache"), "no-store");
    assert_eq!(call(&mock, 0, "headers"), "accept: application/json");
    assert_eq!(call(&mock, 0, "credentials"), "omit");
    assert_eq!(call(&mock, 0, "referrerPolicy"), "no-referrer");

    // The first read is the head alone, as the native devices (and `web_request`) expect.
    let mut head = [0; 4096];
    let count = read(&device, &mut context, &mut head).await.unwrap();
    assert_eq!(
        &head[..count],
        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n"
    );
    let body = read_all(&device, &mut context, 7).await.unwrap();
    assert_eq!(body, br#"{"results":[{"name":"Paris"}]}"#);
    // The end of the response leaves the device ready for the next request.
    assert_eq!(read(&device, &mut context, &mut head).await, Ok(0));
    assert_eq!(write(&device, &mut context, GET), Ok(GET.len()));
    device.close(&mut context).unwrap();
}

#[wasm_bindgen_test]
async fn the_head_can_be_read_in_pieces() {
    let mock = mock();
    handler(&mock, "() => mock.stream(['ok'], 200, { 'x-a': '1' })");
    let device = HttpClientDevice::HTTPS;
    let mut context = open(&device);
    write(&device, &mut context, GET).unwrap();
    let mut piece = [0; 5];
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        let count = read(&device, &mut context, &mut piece).await.unwrap();
        head.extend_from_slice(&piece[..count]);
    }
    assert_eq!(head, b"HTTP/1.1 200 OK\r\nx-a: 1\r\n\r\n");
    assert_eq!(read_all(&device, &mut context, 64).await.unwrap(), b"ok");
}

#[wasm_bindgen_test]
async fn the_body_is_pulled_one_chunk_at_a_time() {
    let mock = mock();
    handler(
        &mock,
        "() => mock.stream(Array.from({ length: 64 }, () => 'x'.repeat(1024)), 200, {})",
    );
    let device = HttpClientDevice::HTTPS;
    let mut context = open(&device);
    write(&device, &mut context, GET).unwrap();
    let mut buffer = [0; 100];
    read(&device, &mut context, &mut buffer).await.unwrap(); // head
    assert_eq!(read(&device, &mut context, &mut buffer).await, Ok(100));
    // 64 KiB are waiting in the browser, but only the chunk being read was requested.
    assert_eq!(field(&mock, "pulls").as_f64(), Some(1.0));
    // A reader that stops can close the device; the rest is never fetched.
    device.close(&mut context).unwrap();
    assert!(aborted(&mock, 0));
    assert_eq!(field(&mock, "pulls").as_f64(), Some(1.0));
}

#[wasm_bindgen_test]
async fn cors_filtered_content_length_is_dropped_and_xila_http_reads_full_body() {
    let mock = mock();
    // Under CORS, Content-Encoding is hidden unless exposed while Content-Length
    // stays visible with the compressed wire length (shorter than the decoded body).
    let expected =
        br#"{"results":[{"name":"Paris","latitude":48.85341,"longitude":2.3488,"country":"France"}]}"#;
    handler(
        &mock,
        r#"() => mock.stream(['{"results":[{"name":"Paris",', '"latitude":48.85341,"longitude":2.3488,"country":"France"}]}'], 200, { 'content-type': 'application/json', 'content-length': '12', 'x-kept': 'y' })"#,
    );
    let device = HttpClientDevice::HTTPS;
    let mut context = open(&device);
    write(&device, &mut context, GET).unwrap();
    let mut head = [0; 256];
    let count = read(&device, &mut context, &mut head).await.unwrap();
    assert_eq!(
        &head[..count],
        b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nx-kept: y\r\n\r\n"
    );
    // A guest framing through Core's xila_http must not truncate the decoded body.
    let parsed = parse_response_head::<32>(&head[..count], false)
        .unwrap()
        .expect("complete head");
    assert_eq!(parsed.status, 200);
    assert_eq!(parsed.body, BodyKind::UntilClose);
    let mut decoder = BodyDecoder::new(parsed.body);
    let mut body = Vec::new();
    let mut chunk = [0; 19];
    loop {
        let count = read(&device, &mut context, &mut chunk).await.unwrap();
        if count == 0 {
            decoder.finish().unwrap();
            break;
        }
        let decoded = decoder.decode(&mut chunk[..count]).unwrap();
        body.extend_from_slice(&chunk[..decoded]);
    }
    assert_eq!(body, expected);
}

#[wasm_bindgen_test]
async fn empty_responses_and_head_preserve_content_length() {
    let mock = mock();
    handler(
        &mock,
        "() => new Response(null, { status: 204, headers: { 'content-length': '674' } })",
    );
    let device = HttpClientDevice::HTTPS;
    let mut context = open(&device);
    write(&device, &mut context, GET).unwrap();
    let mut buffer = [0; 256];
    let count = read(&device, &mut context, &mut buffer).await.unwrap();
    assert_eq!(
        &buffer[..count],
        b"HTTP/1.1 204\r\ncontent-length: 674\r\n\r\n"
    );
    assert_eq!(read(&device, &mut context, &mut buffer).await, Ok(0));

    handler(
        &mock,
        "() => new Response(null, { status: 304, headers: { 'content-length': '674' } })",
    );
    write(&device, &mut context, GET).unwrap();
    let count = read(&device, &mut context, &mut buffer).await.unwrap();
    assert_eq!(
        &buffer[..count],
        b"HTTP/1.1 304\r\ncontent-length: 674\r\n\r\n"
    );
    assert_eq!(read(&device, &mut context, &mut buffer).await, Ok(0));

    handler(
        &mock,
        "() => new Response(null, { status: 200, statusText: 'OK', headers: { 'content-length': '674' } })",
    );
    let head_request = b"HEAD /v1/search HTTP/1.1\r\nHost: geocoding-api.open-meteo.com\r\n\r\n";
    write(&device, &mut context, head_request).unwrap();
    let count = read(&device, &mut context, &mut buffer).await.unwrap();
    assert_eq!(
        &buffer[..count],
        b"HTTP/1.1 200 OK\r\ncontent-length: 674\r\n\r\n"
    );
    assert_eq!(read(&device, &mut context, &mut buffer).await, Ok(0));
}

#[wasm_bindgen_test]
async fn post_sends_the_body_and_only_the_safe_headers() {
    let mock = mock();
    handler(&mock, "() => mock.stream([], 201, {})");
    let device = HttpClientDevice::HTTPS;
    let mut context = open(&device);
    let request = b"POST /items HTTP/1.1\r\nHost: api.example.com\r\nConnection: close\r\nContent-Type: text/plain\r\nContent-Length: 5\r\nUser-Agent: guest\r\n\r\nhello";
    write(&device, &mut context, request).unwrap();
    assert_eq!(call(&mock, 0, "url"), "https://api.example.com/items");
    assert_eq!(call(&mock, 0, "method"), "POST");
    assert_eq!(call(&mock, 0, "headers"), "content-type: text/plain");
    assert_eq!(call(&mock, 0, "body"), "hello");
}

struct Counter;
static WAKES: AtomicUsize = AtomicUsize::new(0);
impl Counter {
    fn waker() -> Waker {
        Self::waker_for(&WAKES)
    }

    fn waker_for(counter: &'static AtomicUsize) -> Waker {
        const VTABLE: RawWakerVTable = RawWakerVTable::new(
            |data| RawWaker::new(data, &VTABLE),
            |data| {
                // SAFETY: `data` always points to a `'static AtomicUsize`.
                unsafe { &*(data as *const AtomicUsize) }.fetch_add(1, Ordering::SeqCst);
            },
            |data| {
                // SAFETY: `data` always points to a `'static AtomicUsize`.
                unsafe { &*(data as *const AtomicUsize) }.fetch_add(1, Ordering::SeqCst);
            },
            |_| {},
        );
        unsafe { Waker::from_raw(RawWaker::new(core::ptr::from_ref(counter).cast(), &VTABLE)) }
    }
}

#[wasm_bindgen_test]
async fn a_slow_request_leaves_the_read_pending_and_wakes_the_reader() {
    let mock = mock();
    handler(
        &mock,
        "() => new Promise((resolve) => { mock.finish = () => resolve(mock.stream(['late'], 200, {})); })",
    );
    let device = HttpClientDevice::HTTPS;
    let mut context = open(&device);
    write(&device, &mut context, GET).unwrap();
    let waker = Counter::waker();
    let mut cx = TaskContext::from_waker(&waker);
    let mut buffer = [0; 64];
    let before = WAKES.load(Ordering::SeqCst);
    assert_eq!(
        device.read(&mut context, &mut cx, &mut buffer, 0),
        Poll::Pending
    );
    // A second request while one is in flight is refused, not queued.
    assert_eq!(
        write(&device, &mut context, GET),
        Err(IoErrorKind::ResourceBusy)
    );
    // The browser starts answering later: let the mocked fetch run first.
    for _ in 0..4 {
        JsFuture::from(js_sys::Promise::resolve(&JsValue::NULL))
            .await
            .unwrap();
    }
    assert_eq!(read(&device, &mut context, &mut buffer[..0]).await, Ok(0));
    Function::new_no_args("globalThis.xilaMock.finish()")
        .call0(&JsValue::NULL)
        .unwrap();
    for _ in 0..4 {
        JsFuture::from(js_sys::Promise::resolve(&JsValue::NULL))
            .await
            .unwrap();
    }
    assert_eq!(WAKES.load(Ordering::SeqCst), before + 1);
    let count = match device.read(&mut context, &mut cx, &mut buffer, 0) {
        Poll::Ready(result) => result.unwrap(),
        Poll::Pending => panic!("response is ready"),
    };
    assert!(buffer[..count].starts_with(b"HTTP/1.1 200 OK"));
    assert_eq!(read_all(&device, &mut context, 64).await.unwrap(), b"late");
}

#[wasm_bindgen_test]
async fn network_and_cors_failures_are_input_output_errors_and_recoverable() {
    let mock = mock();
    handler(
        &mock,
        "() => Promise.reject(new TypeError('Failed to fetch'))",
    );
    let device = HttpClientDevice::HTTPS;
    let mut context = open(&device);
    write(&device, &mut context, GET).unwrap();
    let mut buffer = [0; 64];
    assert_eq!(
        read(&device, &mut context, &mut buffer).await,
        Err(IoErrorKind::InputOutput)
    );
    // The error is reported once; the device is then idle and usable again.
    assert_eq!(read(&device, &mut context, &mut buffer).await, Ok(0));
    handler(&mock, "() => mock.stream(['ok'], 200, {})");
    write(&device, &mut context, GET).unwrap();
    read(&device, &mut context, &mut buffer).await.unwrap();
    assert_eq!(read_all(&device, &mut context, 8).await.unwrap(), b"ok");
}

#[wasm_bindgen_test]
async fn a_stream_that_fails_midway_is_an_error_after_the_data_received() {
    let mock = mock();
    handler(&mock, "() => mock.stream(['part', 'more'], 200, {}, 1)");
    let device = HttpClientDevice::HTTPS;
    let mut context = open(&device);
    write(&device, &mut context, GET).unwrap();
    let mut buffer = [0; 64];
    read(&device, &mut context, &mut buffer).await.unwrap();
    assert_eq!(read(&device, &mut context, &mut buffer).await, Ok(4));
    assert_eq!(
        read(&device, &mut context, &mut buffer).await,
        Err(IoErrorKind::InputOutput)
    );
}

#[wasm_bindgen_test]
async fn a_silent_server_times_out_and_the_request_is_aborted() {
    let mock = mock();
    handler(
        &mock,
        "(_, init) => new Promise((_, reject) => {
            init.signal.addEventListener('abort', () => reject(init.signal.reason));
        })",
    );
    let device = HttpClientDevice::new("https", 30);
    let mut context = open(&device);
    write(&device, &mut context, GET).unwrap();
    let mut buffer = [0; 64];
    assert_eq!(
        read(&device, &mut context, &mut buffer).await,
        Err(IoErrorKind::TimedOut)
    );
    assert!(aborted(&mock, 0));
    assert_eq!(read(&device, &mut context, &mut buffer).await, Ok(0));
}

#[wasm_bindgen_test]
async fn a_stalled_body_read_times_out_and_aborts_the_request() {
    let mock = mock();
    handler(
        &mock,
        "() => new Response(new ReadableStream({ pull: () => new Promise(() => {}) }))",
    );
    let device = HttpClientDevice::new("https", 30);
    let mut context = open(&device);
    write(&device, &mut context, GET).unwrap();
    let mut buffer = [0; 64];
    assert!(read(&device, &mut context, &mut buffer).await.unwrap() > 0);
    assert_eq!(
        read(&device, &mut context, &mut buffer).await,
        Err(IoErrorKind::TimedOut)
    );
    assert!(aborted(&mock, 0));
}

#[wasm_bindgen_test]
async fn invalid_requests_fail_at_write_without_calling_fetch() {
    let mock = mock();
    handler(&mock, "() => mock.stream([], 200, {})");
    let device = HttpClientDevice::HTTPS;
    let mut context = open(&device);
    assert_eq!(
        write(&device, &mut context, b"GET / HTTP/1.1\r\n\r\n"),
        Err(IoErrorKind::InvalidArgument)
    );
    assert_eq!(
        write(
            &device,
            &mut context,
            b"GET / HTTP/1.1\r\nHost: a.example\r\nBad Name: x\r\n\r\n"
        ),
        Err(IoErrorKind::InvalidArgument)
    );
    assert_eq!(field(&mock, "calls.length").as_f64(), Some(0.0));
}

#[wasm_bindgen_test]
async fn the_http_device_requests_http_urls() {
    let mock = mock();
    handler(&mock, "() => mock.stream([], 200, {})");
    let device = HttpClientDevice::HTTP;
    let mut context = open(&device);
    write(
        &device,
        &mut context,
        b"GET /ping HTTP/1.1\r\nHost: localhost:8080\r\n\r\n",
    )
    .unwrap();
    assert_eq!(call(&mock, 0, "url"), "http://localhost:8080/ping");
}

async fn yield_microtasks() {
    for _ in 0..4 {
        JsFuture::from(js_sys::Promise::resolve(&JsValue::NULL))
            .await
            .unwrap();
    }
}

#[wasm_bindgen_test]
async fn two_readers_on_cloned_contexts_are_both_woken() {
    static WAKES_A: AtomicUsize = AtomicUsize::new(0);
    static WAKES_B: AtomicUsize = AtomicUsize::new(0);

    let mock = mock();
    handler(
        &mock,
        "() => new Promise((resolve) => { mock.finish = () => resolve(mock.stream(['ok'], 200, {})); })",
    );
    let device = HttpClientDevice::HTTPS;
    let mut first = open(&device);
    let mut second = device.clone_context(&first).unwrap();
    write(&device, &mut first, GET).unwrap();

    let waker_a = Counter::waker_for(&WAKES_A);
    let waker_b = Counter::waker_for(&WAKES_B);
    let mut cx_a = TaskContext::from_waker(&waker_a);
    let mut cx_b = TaskContext::from_waker(&waker_b);
    let mut buffer = [0; 64];

    let before_a = WAKES_A.load(Ordering::SeqCst);
    let before_b = WAKES_B.load(Ordering::SeqCst);
    assert_eq!(
        device.read(&mut first, &mut cx_a, &mut buffer, 0),
        Poll::Pending
    );
    assert_eq!(
        device.read(&mut second, &mut cx_b, &mut buffer, 0),
        Poll::Pending
    );
    yield_microtasks().await;
    Function::new_no_args("globalThis.xilaMock.finish()")
        .call0(&JsValue::NULL)
        .unwrap();
    yield_microtasks().await;
    assert_eq!(WAKES_A.load(Ordering::SeqCst), before_a + 1);
    assert_eq!(WAKES_B.load(Ordering::SeqCst), before_b + 1);

    // Drain the response and verify cancelling one clone only cancels its own waker.
    read(&device, &mut first, &mut buffer).await.unwrap();
    assert_eq!(read_all(&device, &mut second, 64).await.unwrap(), b"ok");

    write(&device, &mut first, GET).unwrap();
    let before_a = WAKES_A.load(Ordering::SeqCst);
    let before_b = WAKES_B.load(Ordering::SeqCst);
    assert_eq!(
        device.read(&mut first, &mut cx_a, &mut buffer, 0),
        Poll::Pending
    );
    assert_eq!(
        device.read(&mut second, &mut cx_b, &mut buffer, 0),
        Poll::Pending
    );
    device.cancel_operation(&mut first, Operation::Read);
    yield_microtasks().await;
    Function::new_no_args("globalThis.xilaMock.finish()")
        .call0(&JsValue::NULL)
        .unwrap();
    yield_microtasks().await;
    assert_eq!(WAKES_A.load(Ordering::SeqCst), before_a);
    assert_eq!(WAKES_B.load(Ordering::SeqCst), before_b + 1);
}

#[wasm_bindgen_test]
async fn dropping_the_context_aborts_an_in_flight_request() {
    let mock = mock();
    handler(&mock, "() => new Promise(() => {})");
    let device = HttpClientDevice::HTTPS;
    let mut context = open(&device);
    write(&device, &mut context, GET).unwrap();
    assert!(!aborted(&mock, 0));
    drop(context);
    assert!(aborted(&mock, 0));
}

#[wasm_bindgen_test]
async fn an_oversized_response_header_is_refused_with_file_too_large() {
    let mock = mock();
    handler(
        &mock,
        "() => mock.stream(['ok'], 200, { 'x-huge': 'a'.repeat(9000) })",
    );
    let device = HttpClientDevice::HTTPS;
    let mut context = open(&device);
    write(&device, &mut context, GET).unwrap();
    let mut buffer = [0; 64];
    assert_eq!(
        read(&device, &mut context, &mut buffer).await,
        Err(IoErrorKind::FileTooLarge)
    );
}

#[wasm_bindgen_test]
async fn an_oversized_reason_phrase_is_refused_before_reading_headers() {
    let mock = mock();
    handler(
        &mock,
        "() => { const response = new Response('ok', { status: 200, statusText: 'a'.repeat(9000) }); mock.headersRead = false; Object.defineProperty(response, 'headers', { get() { mock.headersRead = true; return new Headers(); } }); return response; }",
    );
    let device = HttpClientDevice::HTTPS;
    let mut context = open(&device);
    write(&device, &mut context, GET).unwrap();
    let mut buffer = [0; 64];
    assert_eq!(
        read(&device, &mut context, &mut buffer).await,
        Err(IoErrorKind::FileTooLarge)
    );
    assert_eq!(field(&mock, "headersRead").as_bool(), Some(false));
}

#[wasm_bindgen_test]
async fn invalid_host_or_url_fails_at_write_without_calling_fetch() {
    let mock = mock();
    handler(&mock, "() => mock.stream([], 200, {})");
    let device = HttpClientDevice::HTTPS;
    let mut context = open(&device);
    for request in [
        &b"GET / HTTP/1.1\r\nHost: example.com:99999\r\n\r\n"[..],
        b"GET / HTTP/1.1\r\nHost: example.com:abc\r\n\r\n",
        b"GET / HTTP/1.1\r\nHost: [::1]:99999\r\n\r\n",
        b"GET / HTTP/1.1\r\nHost: [::1\r\n\r\n",
        b"GET / HTTP/1.1\r\nHost: 999.999.999.999\r\n\r\n",
    ] {
        assert_eq!(
            write(&device, &mut context, request),
            Err(IoErrorKind::InvalidArgument),
            "{:?}",
            String::from_utf8_lossy(request)
        );
    }
    assert_eq!(field(&mock, "calls.length").as_f64(), Some(0.0));
}
