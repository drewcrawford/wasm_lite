# Changelog

All notable changes to this project will be documented in this file.

## Unreleased

### Added

- `wasm_lite::fetch` — the Fetch API plus the slice of the Streams API a
  response body needs: `fetch`, `origin`, `RequestInit`, `Headers`, `Response`,
  `ReadableStream`, `ReadableStreamDefaultReader`. Deliberately smaller than the
  `web-sys` surface it replaces: no `Request` (`fetch` takes a URL string), no
  `window()`/`WorkerGlobalScope` downcast (`fetch` and `origin` read off
  `globalThis`, which is both), and chunks arrive as `Vec<u8>` rather than a
  `Uint8Array`. Browser-tested in Firefox and Chrome
  (`crates/wasm_lite_std/tests/fetch.rs`).
- `wasm_lite::websocket` — `WebSocket`, `BinaryType`, `Event`, `MessageEvent`,
  `CloseEvent`. A message's payload comes back as `Vec<u8>` or `String`
  (`MessageEvent::data_bytes`/`data_text`) rather than a `JsValue` to downcast,
  and `onerror` hands back a plain `Event`, which is what browsers actually
  fire. Round-tripped against the runner's echo endpoint in Firefox and Chrome.
- `JsValue::as_bytes()` / `JsValue::from_bytes()` — read an `ArrayBuffer` or
  `Uint8Array` into a `Vec<u8>`, and copy bytes into an unshared `Uint8Array`.
  The copy is not optional: a borrowed `&[u8]` reaches JS as a view over wasm
  memory that is valid only for the call, and in a `+atomics` build it is a
  *shared* view, which `WebSocket.send` and a `fetch` body both reject.
- `JsValue::to_js_string()` and `Display for JsValue` — JS's own `String(v)`, so
  a rejected promise reports `TypeError: …` instead of a value-table index.
  `Debug` still prints the index. Fallible under the hood with a fallback, since
  this is reached from error paths and `String()` throws for an object with no
  `toString`.
- Benchmarking: `#[wasm_lite_bench]`, `Bencher`, and `bench_main!`, driven by the
  runner one page load per benchmark and reported in `cargo bench`'s format.
  `cargo bench` measures; `cargo test --benches` runs each once without
  measuring. Timing is batch-calibrated because `performance.now()` is coarsened
  to 5 µs even under cross-origin isolation — see
  [docs/testing.md](docs/testing.md#benchmark-in-a-browser).
- `Bencher::iter_custom_async` and `async fn` support in `#[wasm_lite_bench]`,
  for work that cannot be driven synchronously (a `requestAnimationFrame` loop,
  an awaited GPU submission) or whose setup should be excluded from the
  measurement. Fails closed the same way the sync path does.
- `wasm_lite_std::sleep_async` (+ `MAX_TIMEOUT`) — non-blocking sleep, usable on
  the browser main thread where `sleep` traps. Owns its closure, so a dropped
  sleep cancels its timer, and chains sleeps longer than `setTimeout`'s 32-bit
  delay.
- `wasm_lite_std::worker_doctest!` — run a *blocking* doctest on a worker, the
  counterpart to `async_doctest!`. A doctest otherwise runs on the main thread,
  where `join`/`park`/`recv_block` trap.
- `wasm_lite::console::{warn, info, debug, trace}`.
- `wasm_lite::timer` — `setTimeout`/`clearTimeout`.
- `wasm_lite::performance::time_origin()` — `performance.timeOrigin`.
- Runner environment variables, now documented in
  [docs/testing.md](docs/testing.md#configure-the-runner): `WASM_LITE_GPU` (a
  real WebGPU adapter in headless Chrome), `WASM_LITE_BROWSER_ARGS`,
  `WASM_LITE_SERVE_DIR` (serve a directory alongside the program; confined to
  that directory — `..`, absolute components and symlinks pointing outside are
  all refused, and the generated routes always win),
  `WASM_LITE_TIMEOUT_SECS`, and `WASM_LITE_RUN_SECONDS` (watch a long-running
  `bin`).
- The `wasm_lite_std` browser suite now runs in **Chrome as well as Firefox**
  (`scripts/wasm32/tests`), with `a_worker_can_spawn_a_worker` and
  `nesting_composes_to_a_third_level` covering nested worker spawn.

### Changed

- The runner serves a WebSocket **echo endpoint** at `/__wl_echo`, so bindings
  to an API that only means anything against a peer can be tested against one.
  Deliberately minimal: no extensions, no subprotocol negotiation.
- The runner's HTTP server honours the request method and the `Range` header. A
  `HEAD` now returns headers without a body; `Range: bytes=a-b` returns 206 with
  `Content-Range`, and an unsatisfiable range returns 416. It previously answered
  every request with a full 200, which made a range-reading client's primary path
  untestable. Suffix (`bytes=-500`) and multi-range requests are refused rather
  than mis-answered.

### Fixed

- **`Instant` was not comparable across threads.** It was raw
  `performance.now()`, which is per-realm — a Web Worker's zero is the moment
  that worker started, so an `Instant` taken on one thread and read on another
  was out by however long the page had been up. `Instant` is `Ord` and deadlines
  cross threads constantly (`join_async`, every `*_timeout`, an executor's
  `poll_after`), so this affected anything threaded. Now
  `performance.timeOrigin + performance.now()`.
- **A Web Worker could not spawn a Web Worker in Chrome.** The spawn returned a
  handle, nothing reported an error, and the thread never ran; joining it blocked
  forever. Chrome fetches a nested worker's module script through its parent, and
  a parent in `Atomics.wait` — which is every blocking primitive — never services
  it. Worker *creation* is now delegated to the main thread. Firefox was
  unaffected, which is why a Firefox-only CI stayed green.
- A spawned worker that fails to start now reports through `onerror` /
  `onmessageerror` instead of failing silently, and a worker bootstrap that
  throws before the closure runs reports through `error` / `unhandledrejection`
  rather than becoming an invisible unhandled rejection.
- A timeout now prints the console the program captured before it hung, on all
  three run shapes (`bin`, test suite, bench suite). Previously it printed only
  "timed out".
- Three held-lock timeout tests raced: a holder that kept the lock for a fixed
  duration while the other side asserted a short attempt failed. They now hold
  until told to release.

## 0.1.0 - 2026-06-30

Initial release.

### Added

- `wasm_lite`, a dependency-light Rust/JavaScript binding system for `wasm32-unknown-unknown`.
- Descriptor-based import, export, class, and test metadata emitted into custom wasm sections.
- Host-side `wasm_lite_codegen` for dependency-free descriptor parsing and generated ES module glue.
- `wasm-lite` CLI and browser runner support for `cargo run`, `cargo test`, and rustdoc doctests through WebDriver.
- Core ABI support for strings, byte slices, vectors, `JsValue` handles, `Option`, `Result`, and sret payloads.
- Proc-macro support for `import!`, `#[export]`, `#[wasm_lite_test]`, and `js_class!`.
- Browser-oriented exports, imports, doctests, test suites, panic reporting, and interop examples.
- Threading, atomics, async execution, worker bootstrap, and `wasm_lite_std` synchronization/time APIs.
- CI, formatting, clippy, docs, and wasm test scripts for release validation.

### Changed

- Moved macro parsing onto a unified `syn`/`quote` build-time implementation while keeping runtime crates dependency-free.
- Improved documentation for the binding model, testing flow, threading/async behavior, interop, and migration story.

