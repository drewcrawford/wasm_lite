// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bindings to the browser's Fetch API, plus the slice of the Streams API a
//! response body needs.
//!
//! This is the `wasm_lite` answer to `web_sys::{Request, Response, Headers,
//! ReadableStreamDefaultReader}` and the `js_sys` pieces those drag along
//! (`Reflect`, `Function`, `Promise`, `Uint8Array`, `global()`). It is
//! deliberately smaller than the surface it replaces — see "Differences from
//! web-sys" below.
//!
//! ```no_run
//! # async fn f() -> Result<(), wasm_lite::JsValue> {
//! use wasm_lite::fetch::{RequestInit, fetch};
//!
//! let init = RequestInit::new();
//! init.set_method("GET");
//! let response = fetch("/program.wasm", &init).await?;
//! assert!(response.ok());
//! let bytes = response.bytes().await?;
//! # Ok(()) }
//! ```
//!
//! Everything here needs an executor to drive the futures; on wasm that is
//! `wasm_lite_std::spawn_local`.
//!
//! # Differences from web-sys
//!
//! * **No `Request` type.** `fetch(input, init)` accepts a URL string directly,
//!   and a `Request` built only to be handed to `fetch` is a step with no
//!   observable effect. Bind `Request` when something needs to *inspect* or
//!   *clone* a request.
//! * **No `global()` / `window()` / `WorkerGlobalScope` split.** `fetch` and
//!   [`origin`] read off `globalThis`, which resolves to the `Window` on the
//!   main thread and the `WorkerGlobalScope` on a worker without a downcast. The
//!   three-branch dance web-sys forces exists because its `fetch` hangs off the
//!   concrete global type, not because the two globals differ here.
//! * **Chunks arrive as `Vec<u8>`, not `Uint8Array`.** A chunk is copied into
//!   wasm memory at the boundary, so there is no JS-side typed array left to
//!   slice or index. Slice the `Vec` instead.
//! * **No `Response::json`.** Nothing here does serde marshalling yet; read
//!   [`Response::text`] and parse it.

use crate::macros::js_handle;
use crate::{JsFuture, JsValue};

mod imp {
    use crate::JsValue;

    crate::import! {
        "globalThis" {
            /// `fetch(input, init)`. The handle is a `Promise`; the synchronous
            /// half throws on a malformed URL or an init object the engine
            /// rejects, which is why it is fallible before it is awaited.
            fn fetch(input: &str, init: &JsValue) -> Result<JsValue, JsValue>;
            /// `globalThis.origin` — the serialized origin of the current
            /// realm. Defined on both `Window` and `WorkerGlobalScope`.
            #[static_getter] fn origin() -> String;
        }
        "Object" {
            /// `new Object()` — the plain object `fetch` takes as its init.
            #[constructor] fn new_object() -> JsValue as "Object";
        }
        "RequestInit" {
            #[setter] fn set_method(this: &JsValue, v: &str) as "method";
            #[setter] fn set_headers(this: &JsValue, v: &JsValue) as "headers";
            #[setter] fn set_body(this: &JsValue, v: &JsValue) as "body";
            #[setter] fn set_mode(this: &JsValue, v: &str) as "mode";
            #[setter] fn set_credentials(this: &JsValue, v: &str) as "credentials";
            #[setter] fn set_cache(this: &JsValue, v: &str) as "cache";
            #[setter] fn set_redirect(this: &JsValue, v: &str) as "redirect";
            #[setter] fn set_signal(this: &JsValue, v: &JsValue) as "signal";
        }
        "Headers" {
            /// `new Headers()`.
            #[constructor] fn new_headers() -> JsValue as "Headers";
            fn append(this: &JsValue, name: &str, value: &str);
            fn set(this: &JsValue, name: &str, value: &str);
            /// A missing header is `null`, which crosses as `None`.
            fn get(this: &JsValue, name: &str) -> Option<String>;
            fn has(this: &JsValue, name: &str) -> bool;
            fn delete(this: &JsValue, name: &str);
        }
        "Response" {
            #[getter] fn status(this: &JsValue) -> u16;
            #[getter] fn ok(this: &JsValue) -> bool;
            #[getter] fn status_text(this: &JsValue) -> String as "statusText";
            #[getter] fn url(this: &JsValue) -> String;
            #[getter] fn redirected(this: &JsValue) -> bool;
            #[getter] fn headers(this: &JsValue) -> JsValue;
            /// `null` for a response with no body (a HEAD, a 204, a 304).
            #[getter] fn body(this: &JsValue) -> Option<JsValue>;
            /// `Promise<ArrayBuffer>`; throws if the body was already consumed.
            fn array_buffer(this: &JsValue) -> Result<JsValue, JsValue> as "arrayBuffer";
            /// `Promise<String>`; throws if the body was already consumed.
            fn text(this: &JsValue) -> Result<JsValue, JsValue>;
        }
        "ReadableStream" {
            /// Throws if the stream is already locked to another reader.
            fn get_reader(this: &JsValue) -> Result<JsValue, JsValue> as "getReader";
        }
        "ReadableStreamDefaultReader" {
            /// `Promise<{value, done}>`.
            fn read(this: &JsValue) -> JsValue;
            /// `Promise<undefined>`; the promise is discarded, so this reports
            /// nothing about whether the underlying source finished tearing
            /// down.
            fn cancel(this: &JsValue) -> JsValue;
        }
        "ReadableStreamReadResult" {
            #[getter] fn done(this: &JsValue) -> bool;
            #[getter] fn value(this: &JsValue) -> JsValue;
        }
        "Uint8Array" {
            /// `chunk.subarray()` — the whole view. The `bytes` return tag is
            /// what actually copies it into wasm memory; `subarray` is just a
            /// no-op call to hang that on.
            fn array_to_vec(this: &JsValue) -> Vec<u8> as "subarray";
        }
        "ArrayBuffer" {
            /// `buffer.slice(0)` — a copy of the whole buffer, which the `bytes`
            /// return tag then copies into wasm memory.
            fn buffer_to_vec(this: &JsValue, begin: u32) -> Vec<u8> as "slice";
        }
    }
}

js_handle! {
    /// The init object handed to [`fetch`] — the plain JS object web-sys spells
    /// `RequestInit`.
    ///
    /// Every field is optional; `fetch` applies its own defaults for the ones
    /// left unset.
    RequestInit;
}

impl RequestInit {
    /// A new, empty init object.
    pub fn new() -> Self {
        RequestInit(imp::new_object())
    }

    /// The HTTP method (`"GET"`, `"HEAD"`, `"POST"`, …).
    pub fn set_method(&self, method: &str) {
        imp::set_method(&self.0, method);
    }

    /// The request headers.
    pub fn set_headers(&self, headers: &Headers) {
        imp::set_headers(&self.0, &headers.0);
    }

    /// The request body, **copied** into a JS `Uint8Array`.
    ///
    /// The copy is not an optimization to remove — see
    /// [`JsValue::from_bytes`]. In short: a borrowed slice is a view over wasm
    /// memory that is valid only for the call, and in a `+atomics` build it is
    /// a *shared* view, which `fetch` refuses.
    ///
    /// Bodies that are not bytes (a `FormData`, a stream) need the underlying
    /// object; reach them through [`RequestInit::as_js`].
    pub fn set_body(&self, body: &[u8]) {
        imp::set_body(&self.0, &JsValue::from_bytes(body));
    }

    /// `"cors"`, `"no-cors"`, `"same-origin"`, or `"navigate"`.
    pub fn set_mode(&self, mode: &str) {
        imp::set_mode(&self.0, mode);
    }

    /// `"omit"`, `"same-origin"`, or `"include"`.
    pub fn set_credentials(&self, credentials: &str) {
        imp::set_credentials(&self.0, credentials);
    }

    /// `"default"`, `"no-store"`, `"reload"`, `"no-cache"`, `"force-cache"`, or
    /// `"only-if-cached"`.
    pub fn set_cache(&self, cache: &str) {
        imp::set_cache(&self.0, cache);
    }

    /// `"follow"`, `"error"`, or `"manual"`.
    pub fn set_redirect(&self, redirect: &str) {
        imp::set_redirect(&self.0, redirect);
    }

    /// An `AbortSignal` to cancel the request.
    pub fn set_signal(&self, signal: &JsValue) {
        imp::set_signal(&self.0, signal);
    }
}

impl Default for RequestInit {
    fn default() -> Self {
        RequestInit::new()
    }
}

js_handle! {
    /// A [`Headers`](https://developer.mozilla.org/docs/Web/API/Headers) list.
    ///
    /// Header names are case-insensitive, and the browser normalizes them, so
    /// `get("Content-Length")` and `get("content-length")` are the same lookup.
    Headers;
}

impl Headers {
    /// An empty header list.
    pub fn new() -> Self {
        Headers(imp::new_headers())
    }

    /// Add a value, keeping any already set for this name.
    pub fn append(&self, name: &str, value: &str) {
        imp::append(&self.0, name, value);
    }

    /// Set a value, replacing any already set for this name.
    pub fn set(&self, name: &str, value: &str) {
        imp::set(&self.0, name, value);
    }

    /// The value for `name`, or `None` if the header is absent.
    ///
    /// Multiple values for one name are joined with `", "`, as the spec
    /// requires.
    pub fn get(&self, name: &str) -> Option<String> {
        imp::get(&self.0, name)
    }

    /// Whether `name` is present.
    pub fn has(&self, name: &str) -> bool {
        imp::has(&self.0, name)
    }

    /// Remove every value for `name`.
    pub fn delete(&self, name: &str) {
        imp::delete(&self.0, name);
    }
}

impl Default for Headers {
    fn default() -> Self {
        Headers::new()
    }
}

js_handle! {
    /// The response to a [`fetch`].
    ///
    /// The body can be consumed exactly once, by whichever of [`Response::body`],
    /// [`Response::bytes`] or [`Response::text`] is called first; the others
    /// then fail.
    Response;
}

impl Response {
    /// The HTTP status code.
    pub fn status(&self) -> u16 {
        imp::status(&self.0)
    }

    /// Whether the status is in `200..=299`.
    ///
    /// Note what this does *not* cover: a 404 is a successful fetch with
    /// `ok() == false`. Only a network-level failure rejects the promise.
    pub fn ok(&self) -> bool {
        imp::ok(&self.0)
    }

    /// The status text (`"OK"`, `"Not Found"`, …).
    ///
    /// HTTP/2 and HTTP/3 do not carry a reason phrase, so this is often empty
    /// even for a perfectly ordinary response. Log the [`status`](Self::status).
    pub fn status_text(&self) -> String {
        imp::status_text(&self.0)
    }

    /// The final URL, after any redirects.
    pub fn url(&self) -> String {
        imp::url(&self.0)
    }

    /// Whether the response went through at least one redirect.
    pub fn redirected(&self) -> bool {
        imp::redirected(&self.0)
    }

    /// The response headers.
    pub fn headers(&self) -> Headers {
        Headers(imp::headers(&self.0))
    }

    /// The body as a stream, or `None` for a response that has none.
    ///
    /// Prefer [`Response::bytes`] unless you need to stop reading early;
    /// streaming exists so a caller wanting the first kilobyte of a large
    /// resource does not have to buffer all of it.
    ///
    /// **`None` is not a portable test for "no content".** The spec says a HEAD
    /// response has a null body, and Firefox agrees, but Chrome hands back an
    /// empty stream — so a check written against either browser passes there
    /// and fails on the other. Ask for the bytes and check that there are none.
    pub fn body(&self) -> Option<ReadableStream> {
        imp::body(&self.0).map(ReadableStream)
    }

    /// The whole body, buffered.
    pub async fn bytes(&self) -> Result<Vec<u8>, JsValue> {
        let promise = imp::array_buffer(&self.0)?;
        let buffer = JsFuture::new(&promise).await?;
        Ok(imp::buffer_to_vec(&buffer, 0))
    }

    /// The whole body, decoded as UTF-8.
    pub async fn text(&self) -> Result<String, JsValue> {
        let promise = imp::text(&self.0)?;
        let text = JsFuture::new(&promise).await?;
        // The promise resolves to a JS string; anything else means the binding
        // is pointed at something that is not a `Response`.
        text.as_string()
            .ok_or_else(|| JsValue::from_str("Response.text() did not resolve to a string"))
    }
}

js_handle! {
    /// A response body, as a
    /// [`ReadableStream`](https://developer.mozilla.org/docs/Web/API/ReadableStream).
    ReadableStream;
}

impl ReadableStream {
    /// Lock the stream and take a reader.
    ///
    /// Fails if the stream is already locked to another reader — a stream has
    /// at most one at a time.
    pub fn get_reader(&self) -> Result<ReadableStreamDefaultReader, JsValue> {
        Ok(ReadableStreamDefaultReader(imp::get_reader(&self.0)?))
    }
}

js_handle! {
    /// A default reader over a [`ReadableStream`] of bytes.
    ReadableStreamDefaultReader;
}

impl ReadableStreamDefaultReader {
    /// The next chunk, or `Ok(None)` at end of stream.
    ///
    /// The chunk is copied into wasm memory, so the JS-side typed array does
    /// not outlive the call.
    pub async fn read(&self) -> Result<Option<Vec<u8>>, JsValue> {
        let promise = imp::read(&self.0);
        let result = JsFuture::new(&promise).await?;
        if imp::done(&result) {
            return Ok(None);
        }
        let chunk = imp::value(&result);
        Ok(Some(imp::array_to_vec(&chunk)))
    }

    /// Stop reading and release the underlying source.
    ///
    /// Fire-and-forget: the returned promise is dropped, so this says nothing
    /// about whether the source finished tearing down.
    pub fn cancel(&self) {
        let _promise = imp::cancel(&self.0);
    }
}

/// Fetch `input`.
///
/// `input` is a URL, absolute or relative to the current document. The future
/// resolves once the *headers* are in; the body is read separately through
/// [`Response`].
///
/// The `Err` is whatever JS threw or rejected with — a `TypeError` for a
/// network failure, CORS refusal, or malformed URL. An HTTP error status is
/// **not** an error here: check [`Response::ok`].
pub async fn fetch(input: &str, init: &RequestInit) -> Result<Response, JsValue> {
    let promise = imp::fetch(input, &init.0)?;
    Ok(Response(JsFuture::new(&promise).await?))
}

/// The serialized origin of the current realm — `"https://example.com"`.
///
/// Reads `globalThis.origin`, which is defined on both the `Window` and a
/// `WorkerGlobalScope`, so this needs no main-thread/worker branch.
///
/// An opaque origin (a sandboxed frame, a `data:` document) serializes to
/// `"null"` — a string, not an absence.
pub fn origin() -> String {
    imp::origin()
}
