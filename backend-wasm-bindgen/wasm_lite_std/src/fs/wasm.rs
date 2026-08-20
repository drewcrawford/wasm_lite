// SPDX-License-Identifier: MIT OR Apache-2.0

use super::Priority;
use js_sys::wasm_bindgen::{JsCast, JsValue};
use std::fmt;
use std::future::Future;
use std::ops::Deref;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Headers, ReadableStreamDefaultReader, Request, RequestInit, Response};

static DEFAULT_ORIGIN: Mutex<Option<&'static str>> = Mutex::new(None);

/// Observation, compiled only when the crate's `logwise` feature is on.
///
/// The same events, names, and per-field policy as the wasm_lite backend --
/// a `logwise` feature that meant something different depending on which
/// backend was underneath would be worse than not having one.
#[cfg(feature = "logwise")]
mod obs {
    use web_sys::Response;

    /// Renders `Response::status_text()` only if a view actually asks for it.
    ///
    /// `status_text` allocates a `String`, which is exactly the work a `detail`
    /// field exists to avoid at a call site nobody is listening to. It has to be
    /// a named binding rather than an inline temporary: the facade's field array
    /// outlives the statement that builds it.
    struct StatusText<'a>(&'a Response);

    impl core::fmt::Display for StatusText<'_> {
        fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            formatter.write_str(&self.0.status_text())
        }
    }

    pub(super) fn request_failed(operation: &'static str, url: &str, response: &Response) {
        let status_text = StatusText(response);
        logwise::event!(
            class: operational,
            severity: error,
            name: "wasm_lite_std.fs.request.failed",
            operation = support(operation),
            status = support(response.status()),
            detail url = local(url),
            detail status_text = local(logwise::ValueRef::display(&status_text)),
        );
    }

    pub(super) fn exists_not_ok(url: &str, response: &Response) {
        let status_text = StatusText(response);
        logwise::event!(
            class: diagnostic,
            severity: debug,
            name: "wasm_lite_std.fs.exists.not_ok",
            status = support(response.status()),
            detail url = local(url),
            detail status_text = local(logwise::ValueRef::display(&status_text)),
        );
    }

    pub(super) fn origin_set(origin: &str) {
        logwise::event!(
            class: operational,
            severity: info,
            name: "wasm_lite_std.fs.origin.set",
            origin = local(origin),
        );
    }
}

#[derive(Debug)]
pub struct File {
    path: String,
    seek_pos: AtomicU64,
}

#[derive(Debug)]
pub struct Data(Box<[u8]>);

#[derive(Debug, Clone)]
pub struct Metadata {
    len: u64,
}

#[derive(Debug)]
pub enum Error {
    Wasm(String),
    HttpStatus(u16),
    NoBody,
    NotFound,
    InvalidPath,
    PositionOverflow,
    Allocation(String),
    InvalidRangeResponse(String),
    CompressedResponse(String),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wasm(message) => write!(formatter, "WASM I/O error: {message}"),
            Self::HttpStatus(status) => write!(formatter, "HTTP status code {status}"),
            Self::NoBody => formatter.write_str("response has no body"),
            Self::NotFound => formatter.write_str("file was not found"),
            Self::InvalidPath => formatter.write_str("file path is not valid UTF-8"),
            Self::PositionOverflow => formatter.write_str("file position is out of range"),
            Self::Allocation(message) => {
                write!(formatter, "could not allocate file buffer: {message}")
            }
            Self::InvalidRangeResponse(message) => {
                write!(formatter, "invalid HTTP range response: {message}")
            }
            Self::CompressedResponse(encoding) => write!(
                formatter,
                "server responded with compressed encoding '{encoding}'; disable compression for this resource"
            ),
        }
    }
}

impl std::error::Error for Error {}

impl From<JsValue> for Error {
    fn from(value: JsValue) -> Self {
        Self::Wasm(value.as_string().unwrap_or_else(|| format!("{value:?}")))
    }
}

struct Bridge<T> {
    output: crate::Mutex<Option<T>>,
    waker: crate::Mutex<Option<Waker>>,
}

struct PinnedOutput<T> {
    bridge: Arc<Bridge<T>>,
}

impl<T: Send> Future for PinnedOutput<T> {
    type Output = T;

    fn poll(self: std::pin::Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        if let Some(output) = self.bridge.output.lock_sync().take() {
            return Poll::Ready(output);
        }
        *self.bridge.waker.lock_sync() = Some(context.waker().clone());
        // Close the race where the producer stored its output after our first
        // check but before the waker was registered.
        if let Some(output) = self.bridge.output.lock_sync().take() {
            self.bridge.waker.lock_sync().take();
            Poll::Ready(output)
        } else {
            Poll::Pending
        }
    }
}

fn pin_current<F>(future: F) -> impl Future<Output = F::Output> + Send
where
    F: Future + 'static,
    F::Output: Send + 'static,
{
    let bridge = Arc::new(Bridge {
        output: crate::Mutex::new(None),
        waker: crate::Mutex::new(None),
    });
    let producer = Arc::clone(&bridge);
    crate::task_begin();
    crate::spawn_local(async move {
        let output = future.await;
        *producer.output.lock_sync() = Some(output);
        if let Some(waker) = producer.waker.lock_sync().take() {
            waker.wake();
        }
        crate::task_finished();
    });
    PinnedOutput { bridge }
}

fn reserve_buffer(len: usize) -> Result<Vec<u8>, Error> {
    let mut data = Vec::new();
    reserve_additional(&mut data, len)?;
    Ok(data)
}

fn reserve_additional(data: &mut Vec<u8>, additional: usize) -> Result<(), Error> {
    data.try_reserve_exact(additional)
        .map_err(|error| Error::Allocation(error.to_string()))
}

fn invalid_range(message: impl Into<String>) -> Error {
    Error::InvalidRangeResponse(message.into())
}

fn partial_response_len(
    value: &str,
    requested_start: u64,
    requested_end: u64,
) -> Result<usize, Error> {
    let mut fields = value.split_whitespace();
    let unit = fields
        .next()
        .ok_or_else(|| invalid_range("missing range unit"))?;
    let range_and_length = fields
        .next()
        .ok_or_else(|| invalid_range("missing byte range"))?;
    if fields.next().is_some() {
        return Err(invalid_range("unexpected whitespace in Content-Range"));
    }
    if !unit.eq_ignore_ascii_case("bytes") {
        return Err(invalid_range(format!("unsupported range unit '{unit}'")));
    }
    let (range, complete_length) = range_and_length
        .split_once('/')
        .ok_or_else(|| invalid_range("missing complete length"))?;
    let (start, end) = range
        .split_once('-')
        .ok_or_else(|| invalid_range("missing byte-range separator"))?;
    let start = start
        .parse::<u64>()
        .map_err(|error| invalid_range(format!("invalid range start: {error}")))?;
    let end = end
        .parse::<u64>()
        .map_err(|error| invalid_range(format!("invalid range end: {error}")))?;
    if start != requested_start {
        return Err(invalid_range(format!(
            "response starts at {start}, but {requested_start} was requested"
        )));
    }
    if end < start {
        return Err(invalid_range("range end precedes its start"));
    }
    if end > requested_end {
        return Err(invalid_range(format!(
            "response ends at {end}, beyond requested byte {requested_end}"
        )));
    }
    if complete_length != "*" {
        let complete_length = complete_length
            .parse::<u64>()
            .map_err(|error| invalid_range(format!("invalid complete length: {error}")))?;
        if end >= complete_length {
            return Err(invalid_range(format!(
                "range end {end} is outside representation length {complete_length}"
            )));
        }
    }
    let len = end
        .checked_sub(start)
        .and_then(|len| len.checked_add(1))
        .ok_or(Error::PositionOverflow)?;
    usize::try_from(len).map_err(|_| Error::PositionOverflow)
}

fn unsatisfied_response_len(value: &str) -> Result<u64, Error> {
    let mut fields = value.split_whitespace();
    let unit = fields
        .next()
        .ok_or_else(|| invalid_range("missing range unit"))?;
    let range_and_length = fields
        .next()
        .ok_or_else(|| invalid_range("missing unsatisfied range"))?;
    if fields.next().is_some() {
        return Err(invalid_range("unexpected whitespace in Content-Range"));
    }
    if !unit.eq_ignore_ascii_case("bytes") {
        return Err(invalid_range(format!("unsupported range unit '{unit}'")));
    }
    let (range, complete_length) = range_and_length
        .split_once('/')
        .ok_or_else(|| invalid_range("missing complete length"))?;
    if range != "*" {
        return Err(invalid_range(
            "416 response contains a satisfiable byte range",
        ));
    }
    complete_length
        .parse::<u64>()
        .map_err(|error| invalid_range(format!("invalid complete length: {error}")))
}

fn open_status(status: u16, ok: bool) -> Result<(), Error> {
    if ok {
        Ok(())
    } else if status == 404 {
        Err(Error::NotFound)
    } else {
        Err(Error::HttpStatus(status))
    }
}

fn reject_compression(encoding: Option<String>) -> Result<(), Error> {
    if let Some(encoding) = encoding
        && !encoding.eq_ignore_ascii_case("identity")
    {
        return Err(Error::CompressedResponse(encoding));
    }
    Ok(())
}

async fn fetch_request(request: Request) -> Result<Response, Error> {
    let global = js_sys::global();
    let fetch = js_sys::Reflect::get(&global, &JsValue::from_str("fetch"))?
        .dyn_into::<js_sys::Function>()?;
    let response = fetch.call1(&global, &request)?;
    let promise = response.dyn_into::<js_sys::Promise>()?;
    JsFuture::from(promise)
        .await?
        .dyn_into::<Response>()
        .map_err(Into::into)
}

async fn request(url: &str, init: &RequestInit) -> Result<Response, Error> {
    fetch_request(Request::new_with_str_and_init(url, init)?).await
}

impl File {
    pub async fn open(path: impl AsRef<Path>, _priority: Priority) -> Result<Self, Error> {
        let path = path.as_ref().to_str().ok_or(Error::InvalidPath)?.to_owned();
        let url = full_path(&path);
        pin_current(async move {
            let init = RequestInit::new();
            init.set_method("HEAD");
            let response = request(&url, &init).await?;
            if !response.ok() {
                #[cfg(feature = "logwise")]
                obs::request_failed("open", &url, &response);
            }
            open_status(response.status(), response.ok())
        })
        .await?;
        Ok(Self {
            path,
            seek_pos: AtomicU64::new(0),
        })
    }

    pub async fn read(&self, len: usize, _priority: Priority) -> Result<Data, Error> {
        if len == 0 {
            return Ok(Data(Box::new([])));
        }
        let seek_pos = self.seek_pos.load(Ordering::Relaxed);
        let requested_len = u64::try_from(len).map_err(|_| Error::PositionOverflow)?;
        let max_byte = seek_pos
            .checked_add(requested_len - 1)
            .ok_or(Error::PositionOverflow)?;
        let url = full_path(&self.path);

        let bytes = pin_current(async move {
            let init = RequestInit::new();
            init.set_method("GET");
            let headers = Headers::new()?;
            headers.set("Range", &format!("bytes={seek_pos}-{max_byte}"))?;
            init.set_headers(&headers);
            let response = request(&url, &init).await?;
            if response.status() == 416 {
                let content_range = response
                    .headers()
                    .get("content-range")?
                    .ok_or_else(|| invalid_range("416 response is missing Content-Range"))?;
                let complete_length = unsatisfied_response_len(&content_range)?;
                if seek_pos < complete_length {
                    return Err(invalid_range(format!(
                        "server rejected satisfiable byte {seek_pos} for a {complete_length}-byte representation"
                    )));
                }
                return Ok(Vec::new());
            }
            if !response.ok() {
                #[cfg(feature = "logwise")]
                obs::request_failed("read", &url, &response);
                return Err(Error::HttpStatus(response.status()));
            }
            if response.status() != 200 && response.status() != 206 {
                return Err(invalid_range(format!(
                    "expected status 200 or 206, got {}",
                    response.status()
                )));
            }
            reject_compression(response.headers().get("content-encoding")?)?;
            let partial_len = if response.status() == 206 {
                let content_range = response
                    .headers()
                    .get("content-range")?
                    .ok_or_else(|| invalid_range("206 response is missing Content-Range"))?;
                Some(partial_response_len(&content_range, seek_pos, max_byte)?)
            } else {
                None
            };
            let mut skip = if response.status() == 200 {
                seek_pos
            } else {
                0
            };
            let body = response.body().ok_or(Error::NoBody)?;
            let reader = body
                .get_reader()
                .dyn_into::<ReadableStreamDefaultReader>()
                .map_err(|value| Error::from(JsValue::from(value)))?;
            let read_limit = partial_len.unwrap_or(len);
            let mut data = reserve_buffer(read_limit)?;

            loop {
                if data.len() >= read_limit {
                    let _ = reader.cancel();
                    break;
                }
                let result = JsFuture::from(reader.read()).await?;
                if js_sys::Reflect::get(&result, &JsValue::from_str("done"))?
                    .as_bool()
                    .unwrap_or(false)
                {
                    break;
                }
                let value = js_sys::Reflect::get(&result, &JsValue::from_str("value"))?;
                if value.is_undefined() {
                    break;
                }
                let chunk = value.dyn_into::<js_sys::Uint8Array>()?.to_vec();
                if skip >= chunk.len() as u64 {
                    skip -= chunk.len() as u64;
                    continue;
                }
                let start = skip as usize;
                skip = 0;
                let end = chunk
                    .len()
                    .min(start.saturating_add(read_limit - data.len()));
                data.extend_from_slice(&chunk[start..end]);
            }
            if let Some(expected) = partial_len
                && data.len() != expected
            {
                return Err(invalid_range(format!(
                    "Content-Range describes {expected} bytes, but the body contained {}",
                    data.len()
                )));
            }
            Ok(data)
        })
        .await?;

        let bytes_read = u64::try_from(bytes.len()).map_err(|_| Error::PositionOverflow)?;
        let new_pos = seek_pos
            .checked_add(bytes_read)
            .ok_or(Error::PositionOverflow)?;
        self.seek_pos.store(new_pos, Ordering::Relaxed);
        Ok(Data(bytes.into_boxed_slice()))
    }

    pub async fn read_all(&self, priority: Priority) -> Result<Data, Error> {
        let len = self.metadata(priority).await?.len();
        let seek_pos = self.seek_pos.load(Ordering::Relaxed);
        let remaining =
            usize::try_from(len.saturating_sub(seek_pos)).map_err(|_| Error::PositionOverflow)?;
        if remaining == 0 {
            return Ok(Data(Box::new([])));
        }
        let first = self.read(remaining, priority).await?;
        if first.is_empty() || first.len() == remaining {
            return Ok(first);
        }
        let mut data = first.0.into_vec();
        let additional = remaining - data.len();
        reserve_additional(&mut data, additional)?;
        while data.len() < remaining {
            let chunk = self.read(remaining - data.len(), priority).await?;
            if chunk.is_empty() {
                break;
            }
            data.extend_from_slice(&chunk);
        }
        Ok(Data(data.into_boxed_slice()))
    }

    pub async fn seek(
        &mut self,
        position: std::io::SeekFrom,
        priority: Priority,
    ) -> Result<u64, Error> {
        let new_pos = match position {
            std::io::SeekFrom::Start(offset) => offset,
            std::io::SeekFrom::End(offset) => self
                .metadata(priority)
                .await?
                .len()
                .checked_add_signed(offset)
                .ok_or_else(|| Error::Wasm("SeekFrom::End out of range".to_string()))?,
            std::io::SeekFrom::Current(offset) => self
                .seek_pos
                .load(Ordering::Relaxed)
                .checked_add_signed(offset)
                .ok_or_else(|| Error::Wasm("SeekFrom::Current out of range".to_string()))?,
        };
        self.seek_pos.store(new_pos, Ordering::Relaxed);
        Ok(new_pos)
    }

    pub async fn metadata(&self, _priority: Priority) -> Result<Metadata, Error> {
        let url = full_path(&self.path);
        pin_current(async move {
            let init = RequestInit::new();
            init.set_method("HEAD");
            let response = request(&url, &init).await?;
            if !response.ok() {
                #[cfg(feature = "logwise")]
                obs::request_failed("metadata", &url, &response);
                return Err(Error::HttpStatus(response.status()));
            }
            reject_compression(response.headers().get("content-encoding")?)?;
            let len = response
                .headers()
                .get("content-length")?
                .ok_or_else(|| Error::Wasm("missing Content-Length header".to_string()))?
                .parse::<u64>()
                .map_err(|error| Error::Wasm(format!("invalid Content-Length header: {error}")))?;
            Ok(Metadata { len })
        })
        .await
    }
}

impl Data {
    pub fn into_boxed_slice(self) -> Box<[u8]> {
        self.0
    }
}
impl AsRef<[u8]> for Data {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}
impl Deref for Data {
    type Target = [u8];
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl PartialEq for Data {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}
impl Eq for Data {}
impl std::hash::Hash for Data {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}
impl Metadata {
    pub fn len(&self) -> u64 {
        self.len
    }
}

fn origin() -> String {
    if let Some(origin) = *DEFAULT_ORIGIN.lock().unwrap() {
        return origin.to_string();
    }
    js_sys::Reflect::get(&js_sys::global(), &JsValue::from_str("origin"))
        .ok()
        .and_then(|origin| origin.as_string())
        .expect("cannot determine origin; call set_default_origin first")
}

fn full_path(path: &str) -> String {
    format!(
        "{}/{}",
        origin().trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

pub async fn exists(path: impl AsRef<Path>, _priority: Priority) -> bool {
    let Some(path) = path.as_ref().to_str() else {
        return false;
    };
    let url = full_path(path);
    pin_current(async move {
        let init = RequestInit::new();
        init.set_method("HEAD");
        match request(&url, &init).await {
            Ok(response) => {
                if !response.ok() {
                    #[cfg(feature = "logwise")]
                    obs::exists_not_ok(&url, &response);
                }
                response.ok()
            }
            Err(_error) => {
                #[cfg(feature = "logwise")]
                logwise::event!(
                    class: operational,
                    severity: warn,
                    name: "wasm_lite_std.fs.exists.request_failed",
                    detail url = local(url.as_str()),
                    detail error = local(logwise::ValueRef::debug(&_error)),
                );
                false
            }
        }
    })
    .await
}

pub fn set_default_origin(origin: &'static str) {
    #[cfg(feature = "logwise")]
    obs::origin_set(origin);
    *DEFAULT_ORIGIN.lock().unwrap() = Some(origin);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_send<T: Send>(_value: T) {}

    #[test]
    fn range_helpers() {
        assert_eq!(
            partial_response_len("bytes 100-199/1000", 100, 299).unwrap(),
            100
        );
        assert!(partial_response_len("bytes 0-99/1000", 100, 199).is_err());
        assert_eq!(unsatisfied_response_len("bytes */4096").unwrap(), 4096);
        assert!(matches!(open_status(404, false), Err(Error::NotFound)));
        assert!(matches!(
            reject_compression(Some("gzip".to_string())),
            Err(Error::CompressedResponse(_))
        ));
        assert!(reserve_buffer(usize::MAX).is_err());
    }

    #[test]
    fn public_futures_are_send() {
        assert_send(super::super::File::open("/", Priority::unit_test()));
        assert_send(super::super::exists("/", Priority::unit_test()));
    }
}
