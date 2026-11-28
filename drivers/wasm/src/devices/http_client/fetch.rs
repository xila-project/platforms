//! JavaScript glue: `fetch`, deadlines and body streaming.
//!
//! Everything goes through `globalThis`, so it works on the page, in a worker
//! and under test with a replaced `fetch`. Nothing here blocks: each step
//! returns a future the device resolves from the executor.
use super::{request::PreparedRequest, response};
use alloc::{string::String, vec::Vec};
use io_error::{Kind, Result};
use js_sys::{Array, Function, JsString, Object, Promise, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    AbortController, Headers, ReadableStream, ReadableStreamDefaultReader, ReferrerPolicy,
    RequestCache, RequestCredentials, RequestInit, RequestMode, Response,
};

fn global_function(name: &str) -> Result<Function> {
    Reflect::get(&js_sys::global(), &JsValue::from_str(name))
        .ok()
        .and_then(|value| value.dyn_into::<Function>().ok())
        .ok_or(Kind::Unsupported)
}

/// A pending `setTimeout` cleared when dropped.
struct Deadline {
    identifier: JsValue,
    _callback: Closure<dyn FnMut()>,
}

impl Drop for Deadline {
    fn drop(&mut self) {
        if let Ok(clear) = global_function("clearTimeout") {
            let _ = clear.call1(&JsValue::NULL, &self.identifier);
        }
    }
}

/// Wait for `promise`, giving up after `timeout_ms`.
///
/// A local deadline aborts the request and returns [`Kind::TimedOut`].
/// Fetch rejections, including opaque network and CORS failures, return
/// [`Kind::InputOutput`].
async fn within(
    promise: Promise,
    controller: &AbortController,
    timeout_ms: u32,
) -> Result<JsValue> {
    let set_timeout = global_function("setTimeout")?;
    // Only the timer can reject with this object.
    let timeout = JsValue::from(Object::new());
    let mut deadline = None;
    let expired = Promise::new(&mut |_resolve: Function, reject: Function| {
        let timeout = timeout.clone();
        let callback = Closure::once(move || {
            let _ = reject.call1(&JsValue::NULL, &timeout);
        });
        if let Ok(identifier) = set_timeout.call2(
            &JsValue::NULL,
            callback.as_ref(),
            &JsValue::from_f64(f64::from(timeout_ms)),
        ) {
            deadline = Some(Deadline {
                identifier,
                _callback: callback,
            });
        }
    });
    if deadline.is_none() {
        return Err(Kind::InputOutput);
    }
    let result = JsFuture::from(Promise::race(&Array::of2(&promise, &expired))).await;
    drop(deadline);
    result.map_err(|rejection| {
        if rejection == timeout {
            // The timer has won the race. Aborting now cannot mask the deadline
            // with the rejection triggered by aborting Fetch.
            controller.abort();
            Kind::TimedOut
        } else {
            Kind::InputOutput
        }
    })
}

/// The `fetch` options for a request, without its URL.
pub(super) fn options(
    request: &PreparedRequest<'_>,
    controller: &AbortController,
) -> Result<RequestInit> {
    let headers = Headers::new().map_err(|_| Kind::InputOutput)?;
    for (name, value) in &request.headers {
        // Invalid names or values are the guest's mistake, not the network's.
        headers
            .append(name, value)
            .map_err(|_| Kind::InvalidArgument)?;
    }
    let options = RequestInit::new();
    options.set_method(request.method);
    options.set_headers(&headers);
    options.set_mode(RequestMode::Cors);
    // Match the native devices, which never serve cached responses.
    options.set_cache(RequestCache::NoStore);
    // No ambient cookies or HTTP authentication; also keeps `Access-Control-Allow-Origin: *` valid.
    options.set_credentials(RequestCredentials::Omit);
    options.set_referrer_policy(ReferrerPolicy::NoReferrer);
    options.set_signal(Some(&controller.signal()));
    if let Some(body) = request.body {
        options.set_body(&Uint8Array::from(body));
    }
    Ok(options)
}

pub(super) struct Head {
    pub bytes: Vec<u8>,
    pub body: Option<ReadableStreamDefaultReader>,
}

/// Hand the request to the browser. `fetch` starts now; the answer is awaited
/// by [`receive`], so `write` returns while the request is on its way.
pub(super) fn send(url: &str, options: &RequestInit) -> Result<Promise> {
    global_function("fetch")?
        .call2(&JsValue::NULL, &JsValue::from_str(url), options)
        .map_err(|_| Kind::InputOutput)?
        .dyn_into::<Promise>()
        .map_err(|_| Kind::InputOutput)
}

/// Wait for the response and frame its head. The body stays in the browser.
pub(super) async fn receive(
    promise: Promise,
    controller: &AbortController,
    timeout_ms: u32,
    head_request: bool,
) -> Result<Head> {
    let response = within(promise, controller, timeout_ms)
        .await?
        .dyn_into::<Response>()
        .map_err(|_| Kind::InputOutput)?;
    let status_text = Reflect::get(&response, &JsValue::from_str("statusText"))
        .map_err(|_| Kind::InputOutput)?
        .dyn_into::<JsString>()
        .map_err(|_| Kind::InputOutput)?;
    // Check the UTF-16 length before copying from JavaScript so an oversized
    // reason phrase never enters WebAssembly memory; UTF-8 is at least as long.
    if (status_text.length() as usize) > response::MAX_HEAD_SIZE {
        return Err(Kind::FileTooLarge);
    }
    let reason = String::from(status_text);
    let mut size = reason.len();
    if size > response::MAX_HEAD_SIZE {
        return Err(Kind::FileTooLarge);
    }
    let mut headers = Vec::new();
    for entry in js_sys::try_iter(&response.headers())
        .ok()
        .flatten()
        .into_iter()
        .flatten()
    {
        let entry = entry
            .map_err(|_| Kind::InputOutput)?
            .unchecked_into::<Array>();
        if let (Ok(name), Ok(value)) = (
            entry.get(0).dyn_into::<JsString>(),
            entry.get(1).dyn_into::<JsString>(),
        ) {
            // Check the UTF-16 length before copying from JavaScript so an oversized
            // header never enters WebAssembly memory; UTF-8 is at least as long.
            if size
                .saturating_add(name.length() as usize)
                .saturating_add(value.length() as usize)
                .saturating_add(4)
                > response::MAX_HEAD_SIZE
            {
                return Err(Kind::FileTooLarge);
            }
            let name = String::from(name);
            let value = String::from(value);
            size = size
                .saturating_add(name.len())
                .saturating_add(value.len())
                .saturating_add(4);
            if size > response::MAX_HEAD_SIZE {
                return Err(Kind::FileTooLarge);
            }
            headers.push((name, value));
        }
    }
    let bytes = response::build_head(response.status(), &reason, &headers, head_request)?;
    let body = (!head_request)
        .then(|| response.body())
        .flatten()
        .and_then(|body: ReadableStream| body.get_reader().dyn_into().ok());
    Ok(Head { bytes, body })
}

/// The next piece of the body, or `None` at the end of the stream.
pub(super) async fn read_chunk(
    reader: &ReadableStreamDefaultReader,
    controller: &AbortController,
    timeout_ms: u32,
) -> Result<Option<Uint8Array>> {
    let result = within(reader.read(), controller, timeout_ms).await?;
    let done = Reflect::get(&result, &JsValue::from_str("done"))
        .map_err(|_| Kind::InputOutput)?
        .as_bool()
        .unwrap_or(false);
    if done {
        return Ok(None);
    }
    Reflect::get(&result, &JsValue::from_str("value"))
        .ok()
        .and_then(|value| value.dyn_into::<Uint8Array>().ok())
        .map(Some)
        .ok_or(Kind::InputOutput)
}
