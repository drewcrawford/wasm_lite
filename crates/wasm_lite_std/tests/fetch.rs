// SPDX-License-Identifier: MIT OR Apache-2.0
//! Browser suite for `wasm_lite::fetch`, run via the wasm_lite runner.
//!
//! The bindings live in `wasm_lite`, but their tests cannot: awaiting anything
//! needs an executor, the executor is `wasm_lite_std::spawn_local`, and
//! `wasm_lite_std` depends on `wasm_lite` — so a dev-dependency the other way
//! would link two copies of a crate with `#[no_mangle]` exports. This suite is
//! that dependency edge pointing the way it already does.
//!
//! Every test fetches something the runner itself serves — `/program.wasm` is
//! this test binary — so the suite needs no fixture server and no network.
//!
//! ```text
//! RUSTFLAGS="-C target-feature=+atomics,+bulk-memory,+mutable-globals \
//!   -C link-arg=--shared-memory -C link-arg=--max-memory=1073741824 \
//!   -C link-arg=--import-memory -C link-arg=--export=__stack_pointer \
//!   -C link-arg=--export=__tls_base -C link-arg=--export=__tls_size \
//!   -C link-arg=--export=__tls_align -C link-arg=--export=__wasm_init_tls" \
//! CARGO_TARGET_WASM32_UNKNOWN_UNKNOWN_RUNNER=$PWD/target/debug/runner \
//! cargo +nightly test -p wasm_lite_std --test fetch \
//!   --target wasm32-unknown-unknown -Z build-std=std,panic_abort
//! ```

#[cfg(target_arch = "wasm32")]
wasm_lite::test_main!();

// Host: the suite targets the browser, so there is nothing to run here; a
// trivial main satisfies `harness = false`.
#[cfg(not(target_arch = "wasm32"))]
fn main() {}

#[cfg(target_arch = "wasm32")]
mod suite {
    use wasm_lite::fetch::{Headers, RequestInit, fetch, origin};

    /// A resource the runner always serves: this test binary.
    const SELF_WASM: &str = "/program.wasm";
    /// A route the runner has no reason to serve.
    const ABSENT: &str = "/there-is-no-such-resource-here";

    fn get() -> RequestInit {
        let init = RequestInit::new();
        init.set_method("GET");
        init
    }

    fn head() -> RequestInit {
        let init = RequestInit::new();
        init.set_method("HEAD");
        init
    }

    // --- synchronous surface ------------------------------------------------

    #[wasm_lite::wasm_lite_test]
    fn origin_is_the_page_origin() {
        wasm_lite::set_panic_hook();
        let o = origin();
        // The runner serves over plain HTTP on a loopback address; asserting
        // the scheme is the part that is stable across ports and hosts.
        assert!(
            o.starts_with("http://") || o.starts_with("https://"),
            "origin() should be a serialized http(s) origin, got {o:?}"
        );
        assert!(!o.ends_with('/'), "an origin has no trailing slash: {o:?}");
    }

    #[wasm_lite::wasm_lite_test]
    fn headers_roundtrip() {
        wasm_lite::set_panic_hook();
        let h = Headers::new();
        assert_eq!(h.get("range"), None, "an unset header reads as None");
        assert!(!h.has("range"));

        h.set("Range", "bytes=0-99");
        assert!(h.has("range"), "header names are case-insensitive");
        assert_eq!(h.get("range").as_deref(), Some("bytes=0-99"));

        // `append` accumulates where `set` replaces; the spec joins with ", ".
        h.append("Accept", "text/plain");
        h.append("Accept", "text/html");
        assert_eq!(
            h.get("accept").as_deref(),
            Some("text/plain, text/html"),
            "append should keep both values"
        );
        h.set("Accept", "text/css");
        assert_eq!(h.get("accept").as_deref(), Some("text/css"));

        h.delete("Range");
        assert!(!h.has("range"));
        assert_eq!(h.get("range"), None);
    }

    // --- fetch --------------------------------------------------------------

    #[wasm_lite::wasm_lite_test]
    fn fetch_buffers_the_whole_body() {
        wasm_lite::set_panic_hook();
        wasm_lite_std::async_doctest!(async {
            let response = fetch(SELF_WASM, &get()).await.expect("fetch failed");
            assert!(response.ok(), "status {}", response.status());
            assert_eq!(response.status(), 200);
            assert!(!response.redirected());
            assert!(
                response.url().ends_with(SELF_WASM),
                "url {}",
                response.url()
            );

            let bytes = response.bytes().await.expect("body");
            assert!(bytes.len() > 8, "a wasm module is not 8 bytes");
            // The wasm magic number, which is what makes this an assertion
            // about the bytes rather than about their count.
            assert_eq!(&bytes[..4], b"\0asm", "not a wasm module");
        });
    }

    #[wasm_lite::wasm_lite_test]
    fn fetch_reports_http_errors_as_a_response() {
        wasm_lite::set_panic_hook();
        wasm_lite_std::async_doctest!(async {
            // The distinction that catches people: a 404 is a *successful*
            // fetch. Only a network-level failure rejects.
            let response = fetch(ABSENT, &get()).await.expect("404 is not a rejection");
            assert!(!response.ok());
            assert_eq!(response.status(), 404);
        });
    }

    #[wasm_lite::wasm_lite_test]
    fn head_has_headers_and_no_body() {
        wasm_lite::set_panic_hook();
        wasm_lite_std::async_doctest!(async {
            let response = fetch(SELF_WASM, &head()).await.expect("HEAD failed");
            assert!(response.ok(), "status {}", response.status());

            let len: usize = response
                .headers()
                .get("content-length")
                .expect("content-length")
                .parse()
                .expect("content-length is a number");
            assert!(len > 8);

            // A HEAD delivers no body bytes — the observable proof that
            // `set_method` reached the request.
            //
            // Deliberately *not* `body().is_none()`: Firefox gives a HEAD
            // response a null body and Chrome gives it an empty stream, and
            // asserting the Firefox shape passed there for a full run before
            // Chrome caught it. Byte count is what both agree on.
            let head_bytes = response.bytes().await.expect("HEAD body");
            assert!(head_bytes.is_empty(), "HEAD should deliver no body bytes");

            // ...and the length it reported is the length a GET delivers.
            let body = fetch(SELF_WASM, &get())
                .await
                .expect("GET failed")
                .bytes()
                .await
                .expect("body");
            assert_eq!(body.len(), len, "HEAD content-length disagrees with GET");
        });
    }

    #[wasm_lite::wasm_lite_test]
    fn custom_headers_reach_the_server() {
        wasm_lite::set_panic_hook();
        wasm_lite_std::async_doctest!(async {
            let headers = Headers::new();
            headers.set("Accept", "application/wasm");
            let init = get();
            init.set_headers(&headers);

            // The runner does not echo request headers, so what is verified
            // here is that a request carrying them still round-trips — the
            // failure this catches is a `headers` init field the engine
            // rejects, which throws rather than returning a bad response.
            let response = fetch(SELF_WASM, &init).await.expect("fetch failed");
            assert!(response.ok(), "status {}", response.status());
        });
    }

    // --- streaming ----------------------------------------------------------

    #[wasm_lite::wasm_lite_test]
    fn body_streams_in_chunks() {
        wasm_lite::set_panic_hook();
        wasm_lite_std::async_doctest!(async {
            let response = fetch(SELF_WASM, &get()).await.expect("fetch failed");
            let body = response.body().expect("a GET response has a body");
            let reader = body.get_reader().expect("reader");

            let mut collected = Vec::new();
            let mut chunks = 0usize;
            while let Some(chunk) = reader.read().await.expect("read") {
                assert!(!chunk.is_empty(), "an empty chunk is not end-of-stream");
                collected.extend_from_slice(&chunk);
                chunks += 1;
                // A runaway loop should fail as a test, not as a timeout.
                assert!(chunks < 100_000, "stream never ended");
            }
            assert!(chunks > 0, "no chunks read");
            assert_eq!(&collected[..4], b"\0asm");

            // The streamed bytes are the buffered bytes.
            let buffered = fetch(SELF_WASM, &get())
                .await
                .expect("fetch failed")
                .bytes()
                .await
                .expect("body");
            assert_eq!(collected, buffered, "streamed body differs from buffered");
        });
    }

    #[wasm_lite::wasm_lite_test]
    fn cancelling_a_reader_stops_it() {
        wasm_lite::set_panic_hook();
        wasm_lite_std::async_doctest!(async {
            let response = fetch(SELF_WASM, &get()).await.expect("fetch failed");
            let body = response.body().expect("body");
            let reader = body.get_reader().expect("reader");

            let first = reader.read().await.expect("read").expect("one chunk");
            assert!(!first.is_empty());
            reader.cancel();

            // After cancelling, the stream reports end-of-stream rather than
            // hanging or erroring.
            assert_eq!(reader.read().await.expect("read after cancel"), None);
        });
    }

    #[wasm_lite::wasm_lite_test]
    fn a_body_can_only_be_taken_once() {
        wasm_lite::set_panic_hook();
        wasm_lite_std::async_doctest!(async {
            let response = fetch(SELF_WASM, &get()).await.expect("fetch failed");
            let body = response.body().expect("body");
            let _reader = body.get_reader().expect("first reader");

            // A stream has at most one reader, and the second request for one
            // throws rather than silently sharing.
            let second = body.get_reader();
            assert!(second.is_err(), "a locked stream should refuse a reader");
        });
    }

    #[wasm_lite::wasm_lite_test]
    fn a_bad_url_rejects() {
        wasm_lite::set_panic_hook();
        wasm_lite_std::async_doctest!(async {
            // A scheme the browser cannot fetch: this is the network-level
            // failure that *does* reject, as opposed to a 404.
            let result = fetch("not-a-scheme://nowhere", &get()).await;
            assert!(result.is_err(), "an unfetchable URL should reject");
        });
    }
}
