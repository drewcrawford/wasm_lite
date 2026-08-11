// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bindings to the browser's [WebSocket API].
//!
//! This is the `wasm_lite` answer to `web_sys::{WebSocket, BinaryType,
//! MessageEvent, CloseEvent}` and the `js_sys` pieces those drag along
//! (`ArrayBuffer`, `Uint8Array`).
//!
//! ```
//! # #[cfg(target_arch = "wasm32")]
//! # fn run() -> Result<(), wasm_lite::JsValue> {
//! use wasm_lite::websocket::{BinaryType, MessageEvent, WebSocket};
//!
//! let socket = WebSocket::new("wss://example.invalid/socket")?;
//! socket.set_binary_type(BinaryType::ArrayBuffer);
//!
//! let on_message = wasm_lite::Closure::new_with_arg(|event| {
//!     let event = MessageEvent::from_js(event);
//!     if let Some(bytes) = event.data_bytes() {
//!         // a binary frame
//!         let _ = bytes;
//!     }
//! });
//! socket.set_onmessage(Some(on_message.as_js_value()));
//! on_message.forget(); // hand it to JS for the life of the realm
//! # socket.close()?;
//! # Ok(()) }
//! # #[cfg(target_arch = "wasm32")]
//! # fn main() { wasm_lite::set_panic_hook(); run().unwrap(); }
//! # #[cfg(not(target_arch = "wasm32"))]
//! # fn main() {}
//! ```
//!
//! # Handler lifetime
//!
//! A handler is an ordinary JS function value, which in practice means a
//! [`Closure`](crate::Closure). **The caller owns it.** Dropping the `Closure`
//! removes it from the registry, and the socket's later events then call a
//! no-op rather than reading freed memory — safe, but silent, and the usual
//! cause of "my `onmessage` stopped firing". Keep the `Closure` alive for as
//! long as the socket, or `forget()` it.
//!
//! # Differences from web-sys
//!
//! * **`BinaryType` is set, not read.** Reading it back is possible through
//!   [`WebSocket::as_js`] and has no known use.
//! * **A message's payload is `Vec<u8>` or `String`**, via
//!   [`MessageEvent::data_bytes`] / [`MessageEvent::data_text`], rather than a
//!   `JsValue` the caller downcasts. The raw handle is still there
//!   ([`MessageEvent::data`]) for anything else — a `Blob`, say.
//! * **`onerror` hands back a plain [`Event`]**, not an `ErrorEvent`. That
//!   matches what browsers actually fire for a socket error: the spec says
//!   "fire an event named error", with no message, code, or reason. Code that
//!   reaches for `ErrorEvent::message` here gets `undefined` in every browser —
//!   the reason a connection failed is deliberately not exposed to script.
//!   Whatever went wrong shows up on the following `close` event's
//!   [`CloseEvent::code`].
//!
//! [WebSocket API]: https://developer.mozilla.org/docs/Web/API/WebSocket

use crate::JsValue;
use crate::macros::js_handle;

pub use crate::event::Event;

mod imp {
    use crate::JsValue;

    crate::import! {
        "WebSocket" {
            /// `new WebSocket(url)` — throws for a URL that is not a valid
            /// `ws:`/`wss:` absolute URL, or one with a fragment.
            #[constructor] fn new_socket(url: &str) -> Result<JsValue, JsValue> as "WebSocket";
            /// `new WebSocket(url, protocols)`.
            #[constructor] fn new_socket_with_protocol(url: &str, protocol: &str)
                -> Result<JsValue, JsValue> as "WebSocket";

            #[getter] fn ready_state(this: &JsValue) -> u16 as "readyState";
            #[getter] fn url(this: &JsValue) -> String;
            #[getter] fn protocol(this: &JsValue) -> String;
            #[getter] fn buffered_amount(this: &JsValue) -> f64 as "bufferedAmount";
            #[setter] fn set_binary_type(this: &JsValue, v: &str) as "binaryType";

            #[setter] fn set_onopen(this: &JsValue, v: Option<&JsValue>) as "onopen";
            #[setter] fn set_onmessage(this: &JsValue, v: Option<&JsValue>) as "onmessage";
            #[setter] fn set_onerror(this: &JsValue, v: Option<&JsValue>) as "onerror";
            #[setter] fn set_onclose(this: &JsValue, v: Option<&JsValue>) as "onclose";

            /// `socket.send(uint8Array)`.
            ///
            /// Takes an owned handle rather than a `&[u8]`, because a borrowed
            /// slice reaches JS as a view over wasm memory and in a `+atomics`
            /// build that memory is a `SharedArrayBuffer` — which `send`
            /// refuses ("The provided ArrayBufferView value must not be
            /// shared"). See [`JsValue::from_bytes`](crate::JsValue::from_bytes).
            fn send_bytes(this: &JsValue, data: &JsValue) -> Result<(), JsValue> as "send";
            fn send_text(this: &JsValue, data: &str) -> Result<(), JsValue> as "send";
            /// `socket.close(code, reason)`; throws for a code outside the
            /// permitted set or a reason over 123 UTF-8 bytes.
            fn close_with(this: &JsValue, code: u16, reason: &str) -> Result<(), JsValue> as "close";
            fn close(this: &JsValue) -> Result<(), JsValue>;

            /// The `readyState` constants, read off the class rather than
            /// assumed. [`WebSocket::CONNECTING`] and friends are the compiled-in
            /// copies; `ready_state_constants_match_the_browser` asserts the two
            /// agree.
            #[static_getter] fn connecting() -> u16 as "CONNECTING";
            #[static_getter] fn open() -> u16 as "OPEN";
            #[static_getter] fn closing() -> u16 as "CLOSING";
            #[static_getter] fn closed() -> u16 as "CLOSED";
        }
        "MessageEvent" {
            #[getter] fn data(this: &JsValue) -> JsValue;
            #[getter] fn origin(this: &JsValue) -> String;
        }
        "CloseEvent" {
            #[getter] fn code(this: &JsValue) -> u16;
            #[getter] fn reason(this: &JsValue) -> String;
            #[getter] fn was_clean(this: &JsValue) -> bool as "wasClean";
        }
    }
}

/// How binary frames are delivered to `onmessage`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum BinaryType {
    /// An `ArrayBuffer` — what [`MessageEvent::data_bytes`] reads.
    ArrayBuffer,
    /// A `Blob`, whose contents are only reachable asynchronously. The default,
    /// and almost never what Rust code wants.
    #[default]
    Blob,
}

impl BinaryType {
    fn as_str(self) -> &'static str {
        match self {
            BinaryType::ArrayBuffer => "arraybuffer",
            BinaryType::Blob => "blob",
        }
    }
}

js_handle! {
    /// A [`WebSocket`](https://developer.mozilla.org/docs/Web/API/WebSocket).
    ///
    /// Dropping this handle does **not** close the socket: it frees one
    /// reference into the value table, and the browser keeps the connection as
    /// long as JS can still reach it (a registered handler is enough). Call
    /// [`WebSocket::close`].
    WebSocket;
}

impl WebSocket {
    /// The socket has not finished connecting.
    pub const CONNECTING: u16 = 0;
    /// The socket is connected and can carry traffic.
    pub const OPEN: u16 = 1;
    /// The closing handshake has started.
    pub const CLOSING: u16 = 2;
    /// The connection is closed, or was never established.
    pub const CLOSED: u16 = 3;

    /// Open a socket to `url`.
    ///
    /// Fails for a URL the constructor cannot map to `ws:`/`wss:`, or one with
    /// a fragment.
    ///
    /// Two things it does **not** fail for, both of which look like they
    /// should:
    ///
    /// * **A server that is not there.** The constructor returns immediately in
    ///   the [`CONNECTING`](Self::CONNECTING) state; a refused connection
    ///   arrives later as an `error` event followed by a `close`.
    /// * **An `http:`/`https:` URL.** The URL Standard maps those onto
    ///   `ws:`/`wss:`, so `WebSocket::new("http://…")` connects rather than
    ///   rejecting. A relative URL likewise resolves against the page. Neither
    ///   is a place to look for input validation.
    pub fn new(url: &str) -> Result<WebSocket, JsValue> {
        Ok(WebSocket(imp::new_socket(url)?))
    }

    /// As [`WebSocket::new`], requesting a subprotocol.
    pub fn new_with_protocol(url: &str, protocol: &str) -> Result<WebSocket, JsValue> {
        Ok(WebSocket(imp::new_socket_with_protocol(url, protocol)?))
    }

    /// One of [`CONNECTING`](Self::CONNECTING), [`OPEN`](Self::OPEN),
    /// [`CLOSING`](Self::CLOSING), [`CLOSED`](Self::CLOSED).
    pub fn ready_state(&self) -> u16 {
        imp::ready_state(&self.0)
    }

    /// The resolved URL the socket was opened with.
    pub fn url(&self) -> String {
        imp::url(&self.0)
    }

    /// The subprotocol the server selected, or `""` if none.
    ///
    /// Empty until the connection opens.
    pub fn protocol(&self) -> String {
        imp::protocol(&self.0)
    }

    /// Bytes queued by [`send_bytes`](Self::send_bytes) and not yet handed to
    /// the network.
    ///
    /// A `f64` because it is a JS Number that can exceed `u32`.
    pub fn buffered_amount(&self) -> f64 {
        imp::buffered_amount(&self.0)
    }

    /// How binary frames reach `onmessage`. Set this **before** the socket
    /// opens; the default is [`BinaryType::Blob`].
    pub fn set_binary_type(&self, binary_type: BinaryType) {
        imp::set_binary_type(&self.0, binary_type.as_str());
    }

    /// Called with an [`Event`] once the connection is established.
    ///
    /// See the module docs: the caller owns the handler's lifetime.
    pub fn set_onopen(&self, handler: Option<&JsValue>) {
        imp::set_onopen(&self.0, handler);
    }

    /// Called with a [`MessageEvent`] for each inbound frame.
    pub fn set_onmessage(&self, handler: Option<&JsValue>) {
        imp::set_onmessage(&self.0, handler);
    }

    /// Called with an [`Event`] when the connection fails.
    ///
    /// Carries no detail — see the module docs. The reason is on the `close`
    /// event that always follows.
    pub fn set_onerror(&self, handler: Option<&JsValue>) {
        imp::set_onerror(&self.0, handler);
    }

    /// Called with a [`CloseEvent`] when the connection closes, whether or not
    /// it ever opened.
    pub fn set_onclose(&self, handler: Option<&JsValue>) {
        imp::set_onclose(&self.0, handler);
    }

    /// Send a binary frame.
    ///
    /// Fails if the socket is still [`CONNECTING`](Self::CONNECTING). Sending
    /// on a closed socket is *not* an error — the bytes are discarded, which is
    /// what the spec requires.
    ///
    /// The bytes are copied into an unshared `Uint8Array` first. That is not
    /// avoidable: in a `+atomics` build a borrowed slice is a view over a
    /// `SharedArrayBuffer`, and `send` rejects shared views outright.
    pub fn send_bytes(&self, data: &[u8]) -> Result<(), JsValue> {
        imp::send_bytes(&self.0, &JsValue::from_bytes(data))
    }

    /// Send a text frame.
    pub fn send_text(&self, data: &str) -> Result<(), JsValue> {
        imp::send_text(&self.0, data)
    }

    /// Start the closing handshake.
    ///
    /// Returns immediately; the socket reaches [`CLOSED`](Self::CLOSED) when
    /// the `close` event fires.
    pub fn close(&self) -> Result<(), JsValue> {
        imp::close(&self.0)
    }

    /// As [`close`](Self::close), with a status code and reason the peer sees.
    ///
    /// Fails for a `code` other than 1000 or 3000–4999, or a `reason` over 123
    /// UTF-8 bytes.
    pub fn close_with(&self, code: u16, reason: &str) -> Result<(), JsValue> {
        imp::close_with(&self.0, code, reason)
    }

    /// The `readyState` constants as this engine defines them.
    ///
    /// The compiled-in [`CONNECTING`](Self::CONNECTING) and friends are what
    /// code should use; this exists so a test can check the two agree rather
    /// than trusting the copy.
    pub fn browser_ready_states() -> [u16; 4] {
        [
            imp::connecting(),
            imp::open(),
            imp::closing(),
            imp::closed(),
        ]
    }
}

js_handle! {
    /// One inbound frame.
    MessageEvent;
}

impl MessageEvent {
    /// The raw payload: a `String` for a text frame, and — with
    /// [`BinaryType::ArrayBuffer`] set — an `ArrayBuffer` for a binary one.
    pub fn data(&self) -> JsValue {
        imp::data(&self.0)
    }

    /// The payload of a binary frame, or `None` if this frame was text.
    ///
    /// Requires [`BinaryType::ArrayBuffer`]; with the default `Blob` the
    /// payload is not an `ArrayBuffer` and this answers `None` for a frame that
    /// really was binary.
    pub fn data_bytes(&self) -> Option<Vec<u8>> {
        self.data().as_bytes()
    }

    /// The payload of a text frame, or `None` if this frame was binary.
    pub fn data_text(&self) -> Option<String> {
        self.data().as_string()
    }

    /// The origin the message came from.
    pub fn origin(&self) -> String {
        imp::origin(&self.0)
    }
}

js_handle! {
    /// The event delivered when a socket closes.
    CloseEvent;
}

impl CloseEvent {
    /// The close code. `1000` is a normal closure; `1006` is the catch-all the
    /// browser synthesizes when the connection dropped without a handshake —
    /// including a connection that was refused outright.
    pub fn code(&self) -> u16 {
        imp::code(&self.0)
    }

    /// The reason the peer gave, or `""`.
    pub fn reason(&self) -> String {
        imp::reason(&self.0)
    }

    /// Whether the connection closed through the handshake rather than being
    /// cut off.
    pub fn was_clean(&self) -> bool {
        imp::was_clean(&self.0)
    }
}
