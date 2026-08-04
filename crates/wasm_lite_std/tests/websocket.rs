// SPDX-License-Identifier: MIT OR Apache-2.0
//! Browser suite for `wasm_lite::websocket`, run via the wasm_lite runner.
//!
//! Here rather than in `wasm_lite` for the same reason the `fetch` suite is:
//! awaiting needs `spawn_local`, and a dev-dependency pointing the other way
//! would link two copies of a crate with `#[no_mangle]` exports.
//!
//! The peer is the runner's own echo endpoint (`/__wl_echo`), so this needs no
//! external server and no network. Without a peer the suite could only reach
//! the constructor and the failure path — `send`, `onmessage`, `binaryType` and
//! the payload accessors are the majority of the API, and none of them can be
//! exercised by a socket that never connects.
//!
//! ```text
//! CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=$PWD/target/debug/runner \
//! cargo +nightly test -p wasm_lite_std --test websocket \
//!   --target wasm32-unknown-unknown -Z build-std=std,panic_abort
//! ```

#[cfg(target_arch = "wasm32")]
wasm_lite::test_main!();

#[cfg(not(target_arch = "wasm32"))]
fn main() {}

#[cfg(target_arch = "wasm32")]
mod suite {
    use wasm_lite::websocket::{BinaryType, CloseEvent, MessageEvent, WebSocket};
    use wasm_lite::{Closure, JsValue};
    use wasm_lite_std::mpsc::{Receiver, Sender};
    use wasm_lite_std::time::{Duration, Instant};

    /// A socket event, reduced to plain data.
    ///
    /// The handlers cannot hand a `JsValue` to the test body: a handle is
    /// `!Send` and only meaningful in the realm that made it. Reading what is
    /// needed inside the handler is also the pin-to-origin discipline the rest
    /// of this stack is written in.
    #[derive(Debug, PartialEq)]
    enum Ev {
        Open,
        Text(String),
        Bytes(Vec<u8>),
        /// A message that was neither text nor `ArrayBuffer` bytes.
        OtherMessage,
        Error,
        Close {
            code: u16,
            clean: bool,
        },
    }

    /// The echo endpoint, on the origin serving this test.
    fn echo_url() -> String {
        let origin = wasm_lite::fetch::origin();
        // `ws:`/`wss:` mirror `http:`/`https:`, and the runner serves plain
        // HTTP, so this is a prefix swap rather than URL parsing.
        let ws = origin
            .strip_prefix("http")
            .map(|rest| format!("ws{rest}"))
            .expect("the runner serves over http");
        format!("{ws}/__wl_echo")
    }

    /// Everything a live socket needs kept alive: the handlers, and the
    /// receiver the test awaits.
    struct Harness {
        socket: WebSocket,
        events: Receiver<Ev>,
        // Handlers are owned here, not `forget()`en: dropping the `Closure`
        // unregisters it, and a test that leaked one would keep answering
        // events after the test that made it had finished.
        _handlers: Vec<Closure>,
    }

    impl Harness {
        /// Connect to `url` with every handler wired.
        fn connect(url: &str) -> Result<Harness, JsValue> {
            let socket = WebSocket::new(url)?;
            socket.set_binary_type(BinaryType::ArrayBuffer);
            let (tx, events) = wasm_lite_std::mpsc::channel::<Ev>();

            let mut handlers = Vec::new();
            handlers.push(on(&tx, |_, tx| tx(Ev::Open)));
            socket.set_onopen(Some(handlers[0].as_js_value()));

            handlers.push(on(&tx, |v, tx| {
                let event = MessageEvent::from_js(v);
                if let Some(text) = event.data_text() {
                    tx(Ev::Text(text));
                } else if let Some(bytes) = event.data_bytes() {
                    tx(Ev::Bytes(bytes));
                } else {
                    tx(Ev::OtherMessage);
                }
            }));
            socket.set_onmessage(Some(handlers[1].as_js_value()));

            handlers.push(on(&tx, |_, tx| tx(Ev::Error)));
            socket.set_onerror(Some(handlers[2].as_js_value()));

            handlers.push(on(&tx, |v, tx| {
                let event = CloseEvent::from_js(v);
                tx(Ev::Close {
                    code: event.code(),
                    clean: event.was_clean(),
                });
            }));
            socket.set_onclose(Some(handlers[3].as_js_value()));

            Ok(Harness {
                socket,
                events,
                _handlers: handlers,
            })
        }

        /// The next event, or a panic naming what we were waiting for.
        async fn next(&mut self, what: &str) -> Ev {
            // A deadline rather than the runner's page timeout, so a hang says
            // *which* event never arrived instead of dumping the console.
            let deadline = Instant::now() + Duration::from_secs(10);
            self.events
                .recv_async_timeout(deadline)
                .await
                .unwrap_or_else(|e| panic!("waiting for {what}: {e:?}"))
        }

        /// Wait for the socket to open, failing on anything else.
        async fn opened(&mut self) {
            let event = self.next("open").await;
            assert_eq!(event, Ev::Open, "socket did not open");
            assert_eq!(self.socket.ready_state(), WebSocket::OPEN);
        }
    }

    /// Build a one-argument handler that reports through `tx`.
    fn on(tx: &Sender<Ev>, mut f: impl FnMut(JsValue, &mut dyn FnMut(Ev)) + 'static) -> Closure {
        let tx = tx.clone();
        Closure::new_with_arg(move |v| {
            let mut send = |e| {
                // The receiver lives as long as the harness; a send that fails
                // means the test already finished, and there is nobody to tell.
                let _ = tx.send_sync(e);
            };
            f(v, &mut send);
        })
    }

    // --- no peer required ---------------------------------------------------

    #[wasm_lite::wasm_lite_test]
    fn ready_state_constants_match_the_browser() {
        wasm_lite::set_panic_hook();
        // The compiled-in constants are what callers use; this is the only
        // thing standing between them and a silent disagreement with the
        // engine, since they cannot be `const`-asserted against a JS property.
        assert_eq!(
            WebSocket::browser_ready_states(),
            [
                WebSocket::CONNECTING,
                WebSocket::OPEN,
                WebSocket::CLOSING,
                WebSocket::CLOSED
            ]
        );
    }

    #[wasm_lite::wasm_lite_test]
    fn a_bad_url_is_rejected_by_the_constructor() {
        wasm_lite::set_panic_hook();
        // A synchronous throw is the one failure mode that is *not* reported
        // through an event, so it is the one a caller has to handle inline.
        //
        // A scheme the constructor has no mapping for...
        assert!(WebSocket::new("ftp://example.com/").is_err());
        // ...and a fragment, which is specifically forbidden — the case a
        // naive "starts with ws://" check would let through.
        assert!(WebSocket::new("ws://example.com/socket#frag").is_err());

        // What is *not* an error, contrary to reasonable expectation: an
        // `http:`/`https:` URL. The URL Standard maps those onto `ws:`/`wss:`,
        // so this succeeds and connects. A relative URL likewise resolves
        // against the page, so "not a url" is a path, not a syntax error.
        let http = WebSocket::new("http://example.com/").expect("http: normalizes to ws:");
        assert!(
            http.url().starts_with("ws://"),
            "expected normalization to ws://, got {}",
            http.url()
        );
        let _ = http.close();
    }

    #[wasm_lite::wasm_lite_test]
    fn a_refused_connection_closes_uncleanly() {
        wasm_lite::set_panic_hook();
        wasm_lite_std::async_doctest!(async {
            // Port 1 is reserved and nothing listens on it.
            let mut h = Harness::connect("ws://127.0.0.1:1/nope").expect("URL is well-formed");
            assert_eq!(h.socket.ready_state(), WebSocket::CONNECTING);

            // An error event, then a close — in that order, per the spec.
            assert_eq!(h.next("error").await, Ev::Error);
            match h.next("close").await {
                Ev::Close { code, clean } => {
                    // 1006 is the code the browser synthesizes when there was
                    // no closing handshake at all. Script is not told why.
                    assert_eq!(code, 1006);
                    assert!(!clean);
                }
                other => panic!("expected a close, got {other:?}"),
            }
            assert_eq!(h.socket.ready_state(), WebSocket::CLOSED);
        });
    }

    // --- against the runner's echo endpoint ---------------------------------

    #[wasm_lite::wasm_lite_test]
    fn a_text_frame_round_trips() {
        wasm_lite::set_panic_hook();
        wasm_lite_std::async_doctest!(async {
            let mut h = Harness::connect(&echo_url()).expect("bad URL");
            h.opened().await;
            assert!(h.socket.url().ends_with("/__wl_echo"));

            h.socket
                .send_text("hello, socket")
                .unwrap_or_else(|e| panic!("send: {e}"));
            assert_eq!(
                h.next("echo").await,
                Ev::Text("hello, socket".to_string()),
                "the echo did not come back as text"
            );

            // Non-ASCII, because the frame length is in bytes and the Rust
            // string is in chars — an off-by-one here would truncate.
            h.socket
                .send_text("héllo — ünicode ✓")
                .unwrap_or_else(|e| panic!("send: {e}"));
            assert_eq!(
                h.next("echo").await,
                Ev::Text("héllo — ünicode ✓".to_string())
            );
        });
    }

    #[wasm_lite::wasm_lite_test]
    fn a_binary_frame_round_trips() {
        wasm_lite::set_panic_hook();
        wasm_lite_std::async_doctest!(async {
            let mut h = Harness::connect(&echo_url()).expect("bad URL");
            h.opened().await;

            let payload: Vec<u8> = (0..=255u8).collect();
            h.socket
                .send_bytes(&payload)
                .unwrap_or_else(|e| panic!("send: {e}"));
            assert_eq!(
                h.next("echo").await,
                Ev::Bytes(payload),
                "the echo did not come back as bytes"
            );

            // Empty is a real frame, not a no-op.
            h.socket
                .send_bytes(&[])
                .unwrap_or_else(|e| panic!("send: {e}"));
            assert_eq!(h.next("echo").await, Ev::Bytes(Vec::new()));
        });
    }

    #[wasm_lite::wasm_lite_test]
    fn a_large_frame_round_trips() {
        wasm_lite::set_panic_hook();
        wasm_lite_std::async_doctest!(async {
            let mut h = Harness::connect(&echo_url()).expect("bad URL");
            h.opened().await;

            // Past 65535, so both the 16-bit and 64-bit frame-length encodings
            // are exercised rather than only the 7-bit one.
            for len in [200usize, 70_000] {
                let payload: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
                h.socket
                    .send_bytes(&payload)
                    .unwrap_or_else(|e| panic!("send: {e}"));
                match h.next("echo").await {
                    Ev::Bytes(got) => {
                        assert_eq!(got.len(), len, "wrong length back for a {len}-byte frame");
                        assert_eq!(got, payload, "payload differs for a {len}-byte frame");
                    }
                    other => panic!("expected bytes, got {other:?}"),
                }
            }
        });
    }

    #[wasm_lite::wasm_lite_test]
    fn binary_type_blob_is_not_bytes() {
        wasm_lite::set_panic_hook();
        wasm_lite_std::async_doctest!(async {
            let mut h = Harness::connect(&echo_url()).expect("bad URL");
            // The default. `data_bytes` cannot read a Blob, and this asserts it
            // answers "not bytes" rather than quietly returning something.
            h.socket.set_binary_type(BinaryType::Blob);
            h.opened().await;

            h.socket
                .send_bytes(&[1, 2, 3])
                .unwrap_or_else(|e| panic!("send: {e}"));
            assert_eq!(
                h.next("echo").await,
                Ev::OtherMessage,
                "with BinaryType::Blob a binary frame is not readable as bytes"
            );
        });
    }

    #[wasm_lite::wasm_lite_test]
    fn close_completes_the_handshake() {
        wasm_lite::set_panic_hook();
        wasm_lite_std::async_doctest!(async {
            let mut h = Harness::connect(&echo_url()).expect("bad URL");
            h.opened().await;

            h.socket.close_with(1000, "done here").expect("close");
            // The closing handshake is in flight: not open, not yet closed.
            assert_eq!(h.socket.ready_state(), WebSocket::CLOSING);

            match h.next("close").await {
                Ev::Close { code, clean } => {
                    assert_eq!(code, 1000, "a requested close should report its code");
                    // The distinction 1006 exists to draw: this one *did*
                    // handshake.
                    assert!(clean, "a completed handshake is a clean close");
                }
                other => panic!("expected a close, got {other:?}"),
            }
            assert_eq!(h.socket.ready_state(), WebSocket::CLOSED);
        });
    }

    #[wasm_lite::wasm_lite_test]
    fn close_rejects_a_code_it_may_not_send() {
        wasm_lite::set_panic_hook();
        wasm_lite_std::async_doctest!(async {
            let mut h = Harness::connect(&echo_url()).expect("bad URL");
            h.opened().await;

            // 1005 and 1006 are reserved for the browser to synthesize; script
            // may only send 1000 or 3000-4999.
            assert!(h.socket.close_with(1006, "").is_err());
            // A reason over 123 UTF-8 bytes is likewise refused.
            assert!(h.socket.close_with(1000, &"x".repeat(124)).is_err());
            // ...and the socket survived both refusals.
            assert_eq!(h.socket.ready_state(), WebSocket::OPEN);

            h.socket.close().expect("close");
        });
    }

    #[wasm_lite::wasm_lite_test]
    fn sending_before_open_fails() {
        wasm_lite::set_panic_hook();
        wasm_lite_std::async_doctest!(async {
            let mut h = Harness::connect(&echo_url()).expect("bad URL");
            // Still CONNECTING: `send` throws rather than queueing, which is
            // the trap in every "just call send after new" example.
            assert_eq!(h.socket.ready_state(), WebSocket::CONNECTING);
            assert!(h.socket.send_text("too early").is_err());

            // And it works once open, so the failure was the state, not the API.
            h.opened().await;
            h.socket
                .send_text("on time")
                .unwrap_or_else(|e| panic!("send: {e}"));
            assert_eq!(h.next("echo").await, Ev::Text("on time".to_string()));
        });
    }
}
