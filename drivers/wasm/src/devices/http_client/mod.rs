//! Browser `http_client` and `https_client` devices backed by `fetch`.
//!
//! The protocol is the native devices' (see `drivers_shared::devices`): one
//! `write` carries the raw HTTP/1.1 request, then `read` returns the response
//! head first, followed by the decoded body, and `Ok(0)` at its end. Reads
//! return `Poll::Pending` while the browser works, so a guest's synchronous
//! loop with a timeout keeps running and the main thread is never blocked.
//!
//! Memory is bounded: the head is copied once (at most 8 KiB, like the native
//! response buffer) and the body is pulled from the browser's stream one chunk
//! at a time, only when the guest asks for more. A chunk lives in the JavaScript
//! heap and is copied straight into the guest's buffer; nothing accumulates in
//! WebAssembly memory.
//!
//! Browser limits that remain visible to guests: the server must allow
//! cross-origin requests (CORS), redirects are followed by the browser instead
//! of being returned, plain `http` fails from an `https` page (mixed content),
//! and failures do not say whether DNS, TLS, CORS or the network caused them.
mod fetch;
mod request;
mod response;

use ::alloc::boxed::Box;
use alloc::{rc::Rc, vec::Vec};
use core::cell::RefCell;
use file_system::{
    BaseOperations, CharacterDevice, Context, IoErrorKind, MountOperations, Result, Size,
    WaitQueue, Waiter,
};
use js_sys::Uint8Array;
use wasm_bindgen_futures::spawn_local;
use web_sys::{AbortController, ReadableStreamDefaultReader, Url};

/// Native devices wait 15 seconds for each network operation.
const IO_TIMEOUT_MS: u32 = 15_000;

enum Phase {
    Idle,
    /// The request is in flight.
    Connecting,
    /// The head, then the body, are being delivered.
    Streaming,
    Failed(IoErrorKind),
}

struct Exchange {
    generation: u64,
    phase: Phase,
    timeout_ms: u32,
    head: Vec<u8>,
    head_cursor: usize,
    chunk: Option<(Uint8Array, u32)>,
    reader: Option<ReadableStreamDefaultReader>,
    controller: Option<AbortController>,
    reading: bool,
    body_done: bool,
}

struct SharedExchange {
    state: RefCell<Exchange>,
    waiters: WaitQueue<16>,
}

type Shared = Rc<SharedExchange>;

impl Exchange {
    fn new(timeout_ms: u32) -> Self {
        Self {
            generation: 0,
            phase: Phase::Idle,
            timeout_ms,
            head: Vec::new(),
            head_cursor: 0,
            chunk: None,
            reader: None,
            controller: None,
            reading: false,
            body_done: false,
        }
    }

    /// Drop everything belonging to the current request and abort it.
    fn release(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        if let Some(controller) = self.controller.take() {
            controller.abort();
        }
        self.head = Vec::new();
        self.head_cursor = 0;
        self.chunk = None;
        self.reader = None;
        self.reading = false;
        self.body_done = false;
    }

    fn fail(&mut self, error: IoErrorKind) {
        self.release();
        self.phase = Phase::Failed(error);
    }

    fn begin(&mut self, controller: AbortController) -> Result<u64> {
        if matches!(self.phase, Phase::Connecting | Phase::Streaming) {
            return Err(IoErrorKind::ResourceBusy);
        }
        self.release();
        self.phase = Phase::Connecting;
        self.controller = Some(controller);
        Ok(self.generation)
    }
}

impl Drop for Exchange {
    fn drop(&mut self) {
        self.release();
    }
}

fn start_request(
    shared: &Shared,
    generation: u64,
    response: js_sys::Promise,
    controller: AbortController,
    timeout_ms: u32,
    head_request: bool,
) {
    let shared = Rc::downgrade(shared);
    spawn_local(async move {
        let result = fetch::receive(response, &controller, timeout_ms, head_request).await;
        let Some(shared) = shared.upgrade() else {
            return;
        };
        let wake = {
            let mut exchange = shared.state.borrow_mut();
            if exchange.generation != generation {
                false
            } else {
                match result {
                    Ok(head) => {
                        exchange.head = head.bytes;
                        exchange.body_done = head.body.is_none();
                        exchange.reader = head.body;
                        exchange.phase = Phase::Streaming;
                    }
                    Err(error) => exchange.fail(error),
                }
                true
            }
        };
        if wake {
            shared.waiters.wake_all();
        }
    });
}

fn start_read(shared: &Shared, exchange: &mut Exchange) {
    let (Some(reader), Some(controller)) = (exchange.reader.clone(), exchange.controller.clone())
    else {
        return;
    };
    exchange.reading = true;
    let generation = exchange.generation;
    let timeout_ms = exchange.timeout_ms;
    let shared = Rc::downgrade(shared);
    spawn_local(async move {
        let result = fetch::read_chunk(&reader, &controller, timeout_ms).await;
        let Some(shared) = shared.upgrade() else {
            return;
        };
        let wake = {
            let mut exchange = shared.state.borrow_mut();
            if exchange.generation != generation {
                false
            } else {
                exchange.reading = false;
                match result {
                    Ok(Some(chunk)) => exchange.chunk = Some((chunk, 0)),
                    Ok(None) => exchange.body_done = true,
                    Err(error) => exchange.fail(error),
                }
                true
            }
        };
        if wake {
            shared.waiters.wake_all();
        }
    });
}

fn poll_read(
    client: &mut HttpClientContext,
    cx: &mut core::task::Context<'_>,
    buffer: &mut [u8],
) -> core::task::Poll<Result<usize>> {
    use core::task::Poll;
    if buffer.is_empty() {
        return Poll::Ready(Ok(0));
    }
    let result = {
        let mut exchange = client.exchange.state.borrow_mut();
        loop {
            match exchange.phase {
                Phase::Idle => break Poll::Ready(Ok(0)),
                Phase::Failed(error) => {
                    exchange.phase = Phase::Idle;
                    break Poll::Ready(Err(error));
                }
                Phase::Connecting => break Poll::Pending,
                Phase::Streaming => {
                    if exchange.head_cursor < exchange.head.len() {
                        let start = exchange.head_cursor;
                        let count = buffer.len().min(exchange.head.len() - start);
                        buffer[..count].copy_from_slice(&exchange.head[start..start + count]);
                        exchange.head_cursor += count;
                        if exchange.head_cursor == exchange.head.len() {
                            exchange.head = Vec::new();
                            exchange.head_cursor = 0;
                        }
                        break Poll::Ready(Ok(count));
                    }
                    if let Some((chunk, offset)) = exchange.chunk.as_mut() {
                        let remaining = chunk.length() - *offset;
                        if remaining != 0 {
                            let count = remaining.min(buffer.len().min(u32::MAX as usize) as u32);
                            chunk
                                .subarray(*offset, *offset + count)
                                .copy_to(&mut buffer[..count as usize]);
                            *offset += count;
                            break Poll::Ready(Ok(count as usize));
                        }
                        exchange.chunk = None;
                        continue;
                    }
                    if exchange.body_done {
                        exchange.release();
                        exchange.phase = Phase::Idle;
                        break Poll::Ready(Ok(0));
                    }
                    if !exchange.reading {
                        start_read(&client.exchange, &mut exchange);
                    }
                    break Poll::Pending;
                }
            }
        }
    };
    if result.is_ready() {
        client.exchange.waiters.cancel(&mut client.waiter);
        return result;
    }
    if let Err(error) = client
        .exchange
        .waiters
        .register(&mut client.waiter, cx.waker())
    {
        return Poll::Ready(Err(error));
    }
    Poll::Pending
}

struct HttpClientContext {
    exchange: Shared,
    waiter: Waiter,
}

impl Drop for HttpClientContext {
    fn drop(&mut self) {
        self.exchange.waiters.cancel(&mut self.waiter);
    }
}

// SAFETY: this crate only exists on `wasm32`, where the runtime is a single
// thread: the browser's. The JavaScript handles in an exchange are never
// observed from another thread.
unsafe impl Send for HttpClientContext {}
unsafe impl Sync for HttpClientContext {}

pub struct HttpClientDevice {
    scheme: &'static str,
    timeout_ms: u32,
}

// SAFETY: plain data; see `HttpClientContext`.
unsafe impl Send for HttpClientDevice {}
unsafe impl Sync for HttpClientDevice {}

impl HttpClientDevice {
    /// Device for `/devices/network/http_client`.
    pub const HTTP: Self = Self::new("http", IO_TIMEOUT_MS);
    /// Device for `/devices/network/https_client`.
    pub const HTTPS: Self = Self::new("https", IO_TIMEOUT_MS);

    /// A device requesting `scheme` URLs (`"http"` or `"https"`) and giving up
    /// on a network operation after `timeout_ms`.
    pub const fn new(scheme: &'static str, timeout_ms: u32) -> Self {
        Self { scheme, timeout_ms }
    }
}

impl BaseOperations for HttpClientDevice {
    fn open(&self, context: &mut Context) -> Result<()> {
        context.set_private_data(Box::new(HttpClientContext {
            exchange: Rc::new(SharedExchange {
                state: RefCell::new(Exchange::new(self.timeout_ms)),
                waiters: WaitQueue::new(),
            }),
            waiter: Waiter::new(),
        }));
        Ok(())
    }

    fn close(&self, context: &mut Context) -> Result<()> {
        if let Some(mut client) = context.take_private_data_of_type::<HttpClientContext>() {
            client.exchange.waiters.cancel(&mut client.waiter);
            {
                let mut exchange = client.exchange.state.borrow_mut();
                exchange.release();
                exchange.phase = Phase::Idle;
            }
            client.exchange.waiters.wake_all();
        }
        Ok(())
    }

    fn read(
        &self,
        context: &mut Context,
        cx: &mut core::task::Context<'_>,
        buffer: &mut [u8],
        _: Size,
    ) -> core::task::Poll<Result<usize>> {
        let client = context
            .get_private_data_mutable_of_type::<HttpClientContext>()
            .ok_or(IoErrorKind::InvalidState)?;
        poll_read(client, cx, buffer)
    }

    fn write(
        &self,
        context: &mut Context,
        _cx: &mut core::task::Context<'_>,
        buffer: &[u8],
        _: Size,
    ) -> core::task::Poll<Result<usize>> {
        file_system::ready(|| {
            let client = context
                .get_private_data_of_type::<HttpClientContext>()
                .ok_or(IoErrorKind::InvalidState)?;
            let prepared = request::prepare(self.scheme, buffer)?;
            Url::new(&prepared.url).map_err(|_| IoErrorKind::InvalidArgument)?;
            let head_request = prepared.method.eq_ignore_ascii_case("HEAD");
            let controller = AbortController::new().map_err(|_| IoErrorKind::InputOutput)?;
            let options = fetch::options(&prepared, &controller)?;
            let mut exchange = client.exchange.state.borrow_mut();
            if matches!(exchange.phase, Phase::Connecting | Phase::Streaming) {
                return Err(IoErrorKind::ResourceBusy);
            }
            let response = fetch::send(&prepared.url, &options)?;
            let generation = exchange.begin(controller.clone())?;
            drop(exchange);
            start_request(
                &client.exchange,
                generation,
                response,
                controller,
                self.timeout_ms,
                head_request,
            );
            Ok(buffer.len())
        })
    }

    fn clone_context(&self, context: &Context) -> Result<Context> {
        let client = context
            .get_private_data_of_type::<HttpClientContext>()
            .ok_or(IoErrorKind::InvalidState)?;
        Ok(Context::new(Some(HttpClientContext {
            exchange: client.exchange.clone(),
            waiter: Waiter::new(),
        })))
    }

    fn cancel_operation(&self, context: &mut Context, operation: file_system::Operation) {
        if operation == file_system::Operation::Read
            && let Some(client) = context.get_private_data_mutable_of_type::<HttpClientContext>()
        {
            client.exchange.waiters.cancel(&mut client.waiter);
        }
    }
}

impl MountOperations for HttpClientDevice {}

impl CharacterDevice for HttpClientDevice {}

#[cfg(test)]
mod tests;
